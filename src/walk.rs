//! Traversal: `ignore::WalkBuilder` setup and deterministic collection.
//!
//! The parallel walker exists for speed; the caller restores determinism by
//! sorting on the relative path. Nothing downstream depends on worker order.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::{WalkBuilder, WalkState};

/// A file that survived traversal and is a candidate for inclusion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Path as handed to the filesystem.
    pub abs: PathBuf,
    /// Path relative to the working directory, `/`-separated.
    pub rel: String,
    /// True when the path was named on the command line rather than found by
    /// walking a directory. Used for priority scoring.
    pub explicit: bool,
    /// Size in bytes, from the walker's metadata.
    pub size: u64,
    /// Directory depth: 0 for a file directly in the working directory.
    pub depth: usize,
}

/// Traversal configuration, borrowed from the parsed CLI.
#[derive(Debug, Clone, Copy)]
pub struct WalkCfg<'a> {
    pub paths: &'a [PathBuf],
    pub hidden: bool,
    pub no_ignore: bool,
    pub follow_symlinks: bool,
    pub include: Option<&'a GlobSet>,
    pub exclude: Option<&'a GlobSet>,
    pub cwd: &'a Path,
}

/// Traversal result: candidates plus non-fatal warnings.
#[derive(Debug, Clone, Default)]
pub struct Walked {
    pub candidates: Vec<Candidate>,
    pub warnings: Vec<String>,
}

/// Compile `--include` / `--exclude` patterns.
///
/// `literal_separator` is enabled so `*` does not cross `/`; a bare `*.rs`
/// still matches at any depth because matching is retried against the
/// basename. That mirrors how `.gitignore` patterns are normally read.
pub fn build_globs(patterns: &[String], kind: &str) -> Result<Option<GlobSet>, String> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .map_err(|e| format!("invalid --{kind} glob `{pattern}`: {e}"))?;
        builder.add(glob);
    }
    builder
        .build()
        .map(Some)
        .map_err(|e| format!("invalid --{kind} globs: {e}"))
}

/// Match `rel` against a glob set, falling back to the basename so that a bare
/// `*.rs` matches `src/main.rs`.
pub fn matches_globs(set: &GlobSet, rel: &str) -> bool {
    if set.is_match(rel) {
        return true;
    }
    let base = crate::budget::basename(rel);
    base != rel && set.is_match(base)
}

/// Collect every file reachable from `cfg.paths`.
pub fn collect(cfg: &WalkCfg) -> Result<Walked> {
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for path in cfg.paths {
        let meta = std::fs::metadata(path)
            .map_err(|e| anyhow!("cannot access `{}`: {e}", path.display()))?;
        if meta.is_dir() {
            dirs.push(path.clone());
            continue;
        }
        if path.file_name().and_then(|name| name.to_str()) == Some(".contextignore") {
            continue;
        }
        let rel = rel_from(cfg.cwd, path);
        if let Some(exclude) = cfg.exclude {
            if matches_globs(exclude, &rel) {
                continue;
            }
        }
        if seen.insert(rel.clone()) {
            candidates.push(Candidate {
                abs: path.clone(),
                depth: depth_of(&rel),
                rel,
                explicit: true,
                size: meta.len(),
            });
        }
    }

    if !dirs.is_empty() {
        walk_dirs(cfg, &dirs, &mut candidates, &mut warnings, &mut seen);
    }

    Ok(Walked {
        candidates,
        warnings,
    })
}

fn walk_dirs(
    cfg: &WalkCfg,
    dirs: &[PathBuf],
    candidates: &mut Vec<Candidate>,
    warnings: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    let mut builder = WalkBuilder::new(&dirs[0]);
    for dir in &dirs[1..] {
        builder.add(dir);
    }
    builder
        .hidden(!cfg.hidden)
        .ignore(!cfg.no_ignore)
        .git_ignore(!cfg.no_ignore)
        .git_global(!cfg.no_ignore)
        .git_exclude(!cfg.no_ignore)
        .parents(!cfg.no_ignore)
        // Without this, .gitignore is only honoured inside a git repository,
        // which would make `ctx` behave differently in a plain directory.
        .require_git(false)
        .follow_links(cfg.follow_symlinks)
        // `.git/` is always skipped, even with --hidden and --no-ignore.
        .filter_entry(|entry| entry.file_name() != ".git");
    if !cfg.no_ignore {
        builder.add_custom_ignore_filename(".contextignore");
    }

    let cwd = cfg.cwd.to_path_buf();
    let include = cfg.include;
    let exclude = cfg.exclude;
    let (tx, rx) = std::sync::mpsc::channel::<Msg>();

    builder.build_parallel().run(|| {
        let tx = tx.clone();
        let cwd = cwd.clone();
        Box::new(move |result| {
            match result {
                Ok(entry) => {
                    let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
                    if is_file && entry.file_name() != ".contextignore" {
                        let rel = rel_from(&cwd, entry.path());
                        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                        let _ = tx.send(Msg::File(entry.path().to_path_buf(), rel, size));
                    }
                }
                Err(err) => {
                    // Symlink loops and unreadable directories arrive here.
                    let _ = tx.send(Msg::Warn(err.to_string()));
                }
            }
            WalkState::Continue
        })
    });
    drop(tx);

    for msg in rx {
        match msg {
            Msg::File(abs, rel, size) => {
                if let Some(exclude) = exclude {
                    if matches_globs(exclude, &rel) {
                        continue;
                    }
                }
                if let Some(include) = include {
                    if !matches_globs(include, &rel) {
                        continue;
                    }
                }
                if seen.insert(rel.clone()) {
                    candidates.push(Candidate {
                        abs,
                        depth: depth_of(&rel),
                        rel,
                        explicit: false,
                        size,
                    });
                }
            }
            Msg::Warn(text) => warnings.push(text),
        }
    }
}

enum Msg {
    File(PathBuf, String, u64),
    Warn(String),
}

/// Directory depth of a relative path: 0 for a file at the root.
fn depth_of(rel: &str) -> usize {
    rel.matches('/').count()
}

/// Relative, `/`-separated path used for display, glob matching and sorting.
///
/// Paths under the working directory are shown relative to it; anything else
/// is shown as given.
fn rel_from(cwd: &Path, path: &Path) -> String {
    let normalised = normalise(path);
    if normalised.is_absolute() {
        match normalised.strip_prefix(cwd) {
            Ok(rest) => to_slash(rest),
            Err(_) => to_slash(&normalised),
        }
    } else {
        to_slash(&normalised)
    }
}

/// Lexically remove `.` components and resolve `..` textually. No filesystem
/// access, so this cannot fail or block.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(windows)]
fn to_slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(not(windows))]
fn to_slash(path: &Path) -> String {
    // On Unix a backslash is an ordinary filename character, so it is left
    // exactly as it is.
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_counts_directory_levels() {
        // Depth counts the directories above the file, not the separators.
        assert_eq!(depth_of("main.rs"), 0);
        assert_eq!(depth_of("src/main.rs"), 1);
        assert_eq!(depth_of("a/b/c.rs"), 2);
        assert_eq!(depth_of("a/b/c/d.rs"), 3);
    }

    #[test]
    fn normalise_removes_dot_segments() {
        assert_eq!(normalise(Path::new("./a/b")), PathBuf::from("a/b"));
        assert_eq!(normalise(Path::new("a/./b")), PathBuf::from("a/b"));
        assert_eq!(normalise(Path::new("a/b/../c")), PathBuf::from("a/c"));
    }

    #[test]
    fn relative_paths_are_used_as_given() {
        let cwd = Path::new("relative-path-is-used-verbatim");
        assert_eq!(rel_from(cwd, Path::new("src/main.rs")), "src/main.rs");
        assert_eq!(rel_from(cwd, Path::new("./src/main.rs")), "src/main.rs");
    }

    #[test]
    fn absolute_paths_under_the_working_directory_are_made_relative() {
        // `/work` is only absolute on Unix, so the roots differ per platform.
        #[cfg(windows)]
        let (cwd, inside, outside) = (r"C:\work", r"C:\work\src\main.rs", r"C:\other\a.rs");
        #[cfg(not(windows))]
        let (cwd, inside, outside) = ("/work", "/work/src/main.rs", "/elsewhere/a.rs");

        let cwd = Path::new(cwd);
        assert_eq!(rel_from(cwd, Path::new(inside)), "src/main.rs");
        // Anything outside the working directory is shown as given, with `/`
        // separators.
        assert_eq!(
            rel_from(cwd, Path::new(outside)),
            to_slash(Path::new(outside))
        );
    }

    #[test]
    fn globs_match_at_any_depth_but_star_stops_at_separators() {
        let set = build_globs(&["*.rs".to_string()], "include")
            .unwrap()
            .expect("set");
        assert!(matches_globs(&set, "main.rs"));
        assert!(matches_globs(&set, "src/deep/main.rs"));
        assert!(!matches_globs(&set, "src/main.py"));
    }

    #[test]
    fn anchored_globs_respect_directory_boundaries() {
        let set = build_globs(&["src/*.rs".to_string()], "include")
            .unwrap()
            .expect("set");
        assert!(matches_globs(&set, "src/main.rs"));
        assert!(!matches_globs(&set, "src/deep/main.rs"));
        assert!(!matches_globs(&set, "other/main.rs"));
    }

    #[test]
    fn double_star_globs_cross_directories() {
        let set = build_globs(&["src/**".to_string()], "exclude")
            .unwrap()
            .expect("set");
        assert!(matches_globs(&set, "src/main.rs"));
        assert!(matches_globs(&set, "src/deep/nested/main.rs"));
        assert!(!matches_globs(&set, "other/main.rs"));
    }

    #[test]
    fn no_patterns_means_no_glob_set() {
        assert!(build_globs(&[], "include").unwrap().is_none());
    }

    #[test]
    fn malformed_globs_are_reported_with_the_pattern() {
        let err = build_globs(&["[".to_string()], "include").unwrap_err();
        assert!(err.contains("--include"), "{err}");
        assert!(err.contains('['), "{err}");
    }
}
