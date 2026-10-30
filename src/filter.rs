//! File-level skip rules.
//!
//! These are the rules that need the file's name (or its bytes) rather than
//! the directory layout. Everything here is a pure function so the rules can
//! be unit-tested without touching a filesystem.

use std::path::{Path, PathBuf};

/// How much of a file is sniffed for NUL bytes.
pub const BINARY_SNIFF: usize = 8 * 1024;

/// A file is treated as binary when more than this fraction of its decoded
/// characters are U+FFFD replacement characters.
pub const REPLACEMENT_RATIO: f64 = 0.01;

/// Lockfiles are skipped by default, because they are huge, mechanical and
/// almost never useful as LLM context.
pub const LOCKFILES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "poetry.lock",
    "Gemfile.lock",
    "composer.lock",
    "go.sum",
    "uv.lock",
];

/// Build/vendor junk that is commonly *not* gitignored.
pub const JUNK_SUFFIXES: &[&str] = &[".min.js", ".min.css", ".map", ".snap"];

/// Exact secret filenames that are denied by default.
pub const SECRET_NAMES: &[&str] = &["credentials.json", ".npmrc", ".pypirc", ".netrc"];

/// Secret filename suffixes.
pub const SECRET_SUFFIXES: &[&str] = &[".pem", ".key", ".p12", ".pfx"];

/// Secret filename prefixes (covers `id_rsa`, `id_rsa.pub`, `id_ed25519`, ...).
pub const SECRET_PREFIXES: &[&str] = &["id_rsa", "id_ed25519"];

/// `.env` templates are not secrets.
pub const SECRET_ENV_ALLOWED: &[&str] = &[".env.example", ".env.sample"];

/// Per-reason skip tallies for the stderr summary. Counts only: a file dropped
/// by the secret rule is never read into the report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SkipCounts {
    pub binary: usize,
    pub too_large: usize,
    pub lockfile: usize,
    pub junk: usize,
    pub secret: usize,
}

impl SkipCounts {
    /// Only the tests use this; the summary prints the per-reason counts.
    #[cfg(test)]
    pub fn total(&self) -> usize {
        self.binary + self.too_large + self.lockfile + self.junk + self.secret
    }

    /// Human-readable breakdown, e.g. `3 binary, 1 too large, 2 lockfiles`.
    /// Reasons with a zero count are omitted.
    pub fn describe(&self) -> Vec<String> {
        let mut parts = Vec::new();
        let mut add = |n: usize, one: &str, many: &str| {
            if n > 0 {
                parts.push(format!("{n} {}", if n == 1 { one } else { many }));
            }
        };
        add(self.binary, "binary", "binary");
        add(self.too_large, "too large", "too large");
        add(self.lockfile, "lockfile", "lockfiles");
        add(self.junk, "junk", "junk");
        add(self.secret, "secret", "secrets");
        parts
    }
}

/// File name as a `&str`, or `""` for non-UTF-8 names (which then match no
/// rule and are not skipped by name).
pub fn file_name(path: &Path) -> &str {
    path.file_name().and_then(|n| n.to_str()).unwrap_or("")
}

/// True for the lockfiles in [`LOCKFILES`].
pub fn is_lockfile_name(name: &str) -> bool {
    LOCKFILES.contains(&name)
}

/// True for generated build artefacts listed in [`JUNK_SUFFIXES`].
pub fn is_junk_name(name: &str) -> bool {
    JUNK_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
}

/// True for likely-secret filenames. `.env` and `.env.*` are secrets, except
/// for the documented templates.
pub fn is_secret_name(name: &str) -> bool {
    if SECRET_ENV_ALLOWED.contains(&name) {
        return false;
    }
    if name == ".env" || name.starts_with(".env.") {
        return true;
    }
    if SECRET_NAMES.contains(&name) {
        return true;
    }
    if SECRET_SUFFIXES.iter().any(|s| name.ends_with(s)) {
        return true;
    }
    SECRET_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Heuristic binary detection.
///
/// A NUL byte anywhere in the first [`BINARY_SNIFF`] bytes is decisive. After
/// that, a file that is not valid UTF-8 is decoded lossily and rejected only
/// when more than [`REPLACEMENT_RATIO`] of its characters are replacements,
/// which tolerates the occasional stray byte in otherwise-textual files.
pub fn looks_binary(bytes: &[u8]) -> bool {
    let sniff_len = bytes.len().min(BINARY_SNIFF);
    if bytes[..sniff_len].contains(&0) {
        return true;
    }
    if std::str::from_utf8(bytes).is_ok() {
        return false;
    }
    let lossy = String::from_utf8_lossy(bytes);
    let replacements = lossy.chars().filter(|c| *c == '\u{FFFD}').count();
    if replacements == 0 {
        return false;
    }
    let total = lossy.chars().count().max(1);
    (replacements as f64 / total as f64) > REPLACEMENT_RATIO
}

/// Normalise CRLF to LF, leaving bare CR alone.
pub fn normalize_newlines(s: String) -> String {
    if !s.contains('\r') {
        return s;
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' && chars.peek() == Some(&'\n') {
            continue;
        }
        out.push(c);
    }
    out
}

/// The first `n` lines of `s`, used for generated-file detection.
pub fn first_lines(s: &str, n: usize) -> String {
    let mut out = String::new();
    for (i, line) in s.lines().enumerate() {
        if i >= n {
            break;
        }
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line);
    }
    out
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Outcome of reading one candidate file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadOutcome {
    /// Decoded text with CRLF already normalised to LF.
    Text(String),
    /// Rejected by the binary rule.
    Binary,
    /// Could not be read at all; the message is surfaced as a warning.
    Failed(String),
}

/// Read and decode a single file, applying the binary rule.
pub fn load_one(path: &Path) -> LoadOutcome {
    match std::fs::read(path) {
        Ok(bytes) => {
            if looks_binary(&bytes) {
                return LoadOutcome::Binary;
            }
            LoadOutcome::Text(normalize_newlines(
                String::from_utf8_lossy(&bytes).into_owned(),
            ))
        }
        Err(err) => LoadOutcome::Failed(err.to_string()),
    }
}

/// Read many files in parallel, preserving the order of `paths`.
///
/// Order is preserved by chunking the input and appending each chunk's results
/// in sequence, so the payload never depends on thread scheduling. Contents are
/// only ever held once: the caller renders each file into its final form and
/// drops the raw text immediately.
pub fn load_many(paths: &[PathBuf]) -> Vec<LoadOutcome> {
    let count = paths.len();
    if count == 0 {
        return Vec::new();
    }
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, count);
    let chunk = count.div_ceil(threads);

    let mut results: Vec<LoadOutcome> = Vec::with_capacity(count);
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for slice in paths.chunks(chunk) {
            handles.push(
                scope.spawn(move || slice.iter().map(|path| load_one(path)).collect::<Vec<_>>()),
            );
        }
        for handle in handles {
            if let Ok(part) = handle.join() {
                results.extend(part);
            }
        }
    });

    if results.len() != count {
        // A worker died, which should be impossible: ctx never panics on user
        // input. Re-read sequentially rather than pairing contents with the
        // wrong paths.
        results.clear();
        results.extend(paths.iter().map(|path| load_one(path)));
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lockfiles_are_recognised() {
        for name in LOCKFILES {
            assert!(is_lockfile_name(name), "{name} should be a lockfile");
        }
        for name in ["Cargo.toml", "lock.json", "go.mod", "uv.lock.bak"] {
            assert!(!is_lockfile_name(name), "{name} is not a lockfile");
        }
    }

    #[test]
    fn junk_suffixes_are_recognised() {
        for name in ["app.min.js", "site.min.css", "bundle.js.map", "x.snap"] {
            assert!(is_junk_name(name), "{name} should be junk");
        }
        for name in ["app.js", "main.rs", "snapshot.rs", "min.js"] {
            assert!(!is_junk_name(name), "{name} should not be junk");
        }
    }

    #[test]
    fn secret_names_are_recognised() {
        for name in [
            ".env",
            ".env.local",
            ".env.production",
            "server.pem",
            "server.key",
            "id_rsa",
            "id_rsa.pub",
            "id_ed25519",
            "keystore.p12",
            "cert.pfx",
            "credentials.json",
            ".npmrc",
            ".pypirc",
            ".netrc",
        ] {
            assert!(is_secret_name(name), "{name} should be a secret");
        }
    }

    #[test]
    fn env_templates_are_not_secrets() {
        assert!(!is_secret_name(".env.example"));
        assert!(!is_secret_name(".env.sample"));
        assert!(is_secret_name(".env.example.local"));
    }

    #[test]
    fn ordinary_files_are_not_secrets() {
        for name in [
            "main.rs",
            "README.md",
            "env",
            "environment.py",
            "keyboard.keymap",
            "keys.rs",
            "monkey.rs",
            "Cargo.toml",
        ] {
            assert!(!is_secret_name(name), "{name} should not be a secret");
        }
    }

    #[test]
    fn nul_byte_in_sniff_window_is_binary() {
        assert!(looks_binary(b"abc\0def"));
        let mut v = vec![b'a'; BINARY_SNIFF + 10];
        v[BINARY_SNIFF + 5] = 0;
        // NUL beyond the sniff window, and the content is valid UTF-8.
        assert!(!looks_binary(&v));
    }

    #[test]
    fn valid_utf8_is_never_binary() {
        assert!(!looks_binary(
            "fn main() { println!(\"héllo → wörld\"); }\n".as_bytes()
        ));
        assert!(!looks_binary(b""));
    }

    #[test]
    fn mostly_invalid_utf8_is_binary() {
        let bytes = [0xffu8, 0xfe, 0xfd, 0xfc, 0xfb, 0xfa];
        assert!(looks_binary(&bytes));
    }

    #[test]
    fn a_few_bad_bytes_in_a_long_text_file_is_not_binary() {
        let mut bytes = vec![b'a'; 4000];
        bytes.push(0xff);
        assert!(!looks_binary(&bytes));
    }

    #[test]
    fn crlf_is_normalised_to_lf() {
        assert_eq!(normalize_newlines("a\r\nb\r\n".to_string()), "a\nb\n");
        assert_eq!(normalize_newlines("a\nb\n".to_string()), "a\nb\n");
        assert_eq!(normalize_newlines("a\rb".to_string()), "a\rb");
        assert_eq!(normalize_newlines(String::new()), "");
    }

    #[test]
    fn normalisation_leaves_clean_input_untouched() {
        let s = "no carriage returns here\nsecond line\n".to_string();
        assert_eq!(normalize_newlines(s.clone()), s);
    }

    #[test]
    fn first_lines_takes_at_most_n() {
        assert_eq!(first_lines("a\nb\nc\nd\n", 2), "a\nb");
        assert_eq!(first_lines("a\n", 5), "a");
        assert_eq!(first_lines("", 5), "");
    }

    #[test]
    fn skip_counts_describe_themselves() {
        let counts = SkipCounts {
            binary: 3,
            too_large: 1,
            lockfile: 2,
            junk: 0,
            secret: 1,
        };
        assert_eq!(
            counts.describe(),
            vec!["3 binary", "1 too large", "2 lockfiles", "1 secret"]
        );
        assert_eq!(counts.total(), 7);
        assert!(SkipCounts::default().describe().is_empty());
    }

    #[test]
    fn file_name_handles_paths_without_a_name() {
        assert_eq!(file_name(&PathBuf::from("a/b/c.rs")), "c.rs");
        assert_eq!(file_name(Path::new("/")), "");
    }
}
