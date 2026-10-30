//! Payload sinks: stdout, a file, and the clipboard.
//!
//! Nothing here ever spawns a process. Clipboard access goes through
//! `arboard`, which talks to the platform clipboard API directly.

use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result};

/// Result of writing the payload to stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdoutOutcome {
    Written,
    /// The reader closed the pipe early, as `ctx | head -c 10` does. This is
    /// not an error and must not produce a message.
    ClosedEarly,
}

/// Result of a clipboard copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyOutcome {
    /// The payload was set successfully on the system clipboard.
    Copied,
}

/// True when an I/O error means "the reader on the other end went away".
pub fn is_broken_pipe(err: &io::Error) -> bool {
    if err.kind() == io::ErrorKind::BrokenPipe {
        return true;
    }
    // Windows reports a closed pipe as ERROR_NO_DATA (232) in some cases.
    matches!(err.raw_os_error(), Some(232))
}

/// Write the payload to stdout, treating a closed pipe as a clean exit.
pub fn write_stdout(payload: &str) -> Result<StdoutOutcome> {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    match lock
        .write_all(payload.as_bytes())
        .and_then(|()| lock.flush())
    {
        Ok(()) => Ok(StdoutOutcome::Written),
        Err(err) if is_broken_pipe(&err) => Ok(StdoutOutcome::ClosedEarly),
        Err(err) => Err(anyhow::Error::new(err).context("failed to write the payload to stdout")),
    }
}

/// Write the payload to a file, replacing it if it exists.
pub fn write_file(path: &Path, payload: &str) -> Result<()> {
    std::fs::write(path, payload.as_bytes())
        .with_context(|| format!("failed to write `{}`", path.display()))
}

/// Put the payload on the system clipboard.
pub fn copy_to_clipboard(payload: &str) -> Result<CopyOutcome> {
    copy_impl(payload)
}

#[cfg(not(target_os = "linux"))]
fn copy_impl(payload: &str) -> Result<CopyOutcome> {
    let mut clipboard = arboard::Clipboard::new().context("cannot open the system clipboard")?;
    clipboard
        .set_text(payload.to_string())
        .context("cannot put the payload on the clipboard")?;
    Ok(CopyOutcome::Copied)
}

/// On Linux the process that owns the X11 selection must stay alive until
/// another clipboard owner replaces the selection, otherwise the copy vanishes.
///
/// The clipboard handle is created *inside* the worker thread on purpose: on
/// X11 the owning connection must outlive the handover. The main thread waits
/// until `arboard` observes the selection being replaced.
#[cfg(target_os = "linux")]
fn copy_impl(payload: &str) -> Result<CopyOutcome> {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    let text = payload.to_string();
    std::thread::spawn(move || {
        let mut clipboard = match arboard::Clipboard::new() {
            Ok(clipboard) => clipboard,
            Err(err) => {
                let _ = tx.send(Err(format!("cannot open the system clipboard: {err}")));
                return;
            }
        };
        use arboard::SetExtLinux;
        let result = clipboard.set().wait().text(text);
        let _ = tx
            .send(result.map_err(|err| format!("cannot put the payload on the clipboard: {err}")));
    });

    match rx.recv() {
        Ok(Ok(())) => Ok(CopyOutcome::Copied),
        Ok(Err(message)) => Err(anyhow::anyhow!("{message}")),
        Err(err) => Err(anyhow::anyhow!(
            "clipboard worker stopped before clipboard ownership changed: {err}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broken_pipe_is_recognised() {
        assert!(is_broken_pipe(&io::Error::new(
            io::ErrorKind::BrokenPipe,
            "closed"
        )));
        assert!(!is_broken_pipe(&io::Error::new(
            io::ErrorKind::PermissionDenied,
            "nope"
        )));
    }

    #[test]
    fn windows_no_data_error_counts_as_a_broken_pipe() {
        let err = io::Error::from_raw_os_error(232);
        assert!(is_broken_pipe(&err));
    }

    #[test]
    fn files_are_written_verbatim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("payload.xml");
        write_file(&path, "<context>\n</context>\n").expect("write");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "<context>\n</context>\n"
        );
    }

    #[test]
    fn writing_to_an_unwritable_path_reports_the_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("no-such-dir").join("payload.xml");
        let err = write_file(&missing, "x").unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("payload.xml"), "{text}");
    }
}
