use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use globset::GlobSet;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::Match;

use crate::cli::Cli;
use crate::filter;
use crate::walk::{self, Candidate};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Changed,
    Added,
}

pub struct Selection {
    pub candidates: Vec<Candidate>,
    pub kinds: BTreeMap<String, ChangeKind>,
    pub deleted: Vec<String>,
}

pub fn select(
    rev: &str,
    cwd: &Path,
    paths: &[PathBuf],
    candidates: Vec<Candidate>,
    cli: &Cli,
    include: Option<&GlobSet>,
    exclude: Option<&GlobSet>,
) -> Result<Selection> {
    let repo = gix::discover(cwd)
        .map_err(|_| anyhow!("not inside a Git repository; --since requires a working tree"))?;
    if matches!(repo.object_hash(), gix::hash::Kind::Sha256) {
        return Err(anyhow!(
            "unsupported Git object format: SHA-256 repositories are not supported"
        ));
    }

    let object = repo
        .rev_parse_single(gix::bstr::BStr::new(rev.as_bytes()))
        .and_then(|id| id.object())
        .and_then(|object| object.peel_to_commit())
        .map_err(|_| {
            anyhow!(
                "cannot resolve revision `{}`; shallow clones may not contain the requested commit",
                one_line(rev)
            )
        })?;
    let tree = object
        .tree()
        .context("cannot read the requested revision tree")?;
    let mut entries = BTreeMap::new();
    for entry in tree.traverse().breadthfirst.files()? {
        if !entry.mode.is_blob_or_symlink() {
            continue;
        }
        let rel = String::from_utf8_lossy(&entry.filepath).into_owned();
        entries.insert(rel, entry.oid);
    }

    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("--since requires a Git working tree"))?;
    let mut changed = Vec::new();
    let mut kinds = BTreeMap::new();
    for candidate in candidates {
        let git_path = git_relative_path(workdir, cwd, &candidate.abs)?;
        match entries.get(&git_path) {
            Some(oid) => {
                let base = repo
                    .find_object(*oid)
                    .with_context(|| format!("cannot read Git blob for `{}`", candidate.rel))?;
                let working = std::fs::read(&candidate.abs)
                    .with_context(|| format!("cannot read `{}`", candidate.rel))?;
                if working != base.data && normalize_crlf(&working) != normalize_crlf(&base.data) {
                    kinds.insert(candidate.rel.clone(), ChangeKind::Changed);
                    changed.push(candidate);
                }
            }
            None => {
                kinds.insert(candidate.rel.clone(), ChangeKind::Added);
                changed.push(candidate);
            }
        }
    }

    let scopes: Vec<PathBuf> = paths
        .iter()
        .map(|path| absolute_lexical(cwd, path))
        .collect();
    let mut deleted = Vec::new();
    for (git_path, oid) in entries {
        let Some(abs) = repo.workdir_path(gix::bstr::BStr::new(git_path.as_bytes())) else {
            continue;
        };
        if abs.exists() || !in_scope(&abs, &scopes) {
            continue;
        }
        if !cli.no_ignore && is_ignored_deleted(&repo, workdir, &abs)? {
            continue;
        }
        let rel = display_path(cwd, &abs);
        if path_is_hidden(&git_path) && !cli.hidden {
            continue;
        }
        if git_path.split('/').any(|part| part == ".git") || git_path == ".contextignore" {
            continue;
        }
        let name = filter::file_name(Path::new(&git_path));
        if !cli.include_lockfiles && filter::is_lockfile_name(name) {
            continue;
        }
        if !cli.include_secrets && filter::is_secret_name(name) {
            continue;
        }
        if let Some(exclude) = exclude {
            if walk::matches_globs(exclude, &rel) {
                continue;
            }
        }
        if include.is_some_and(|set| !walk::matches_globs(set, &rel)) {
            continue;
        }
        if filter::is_junk_name(name)
            && !include
                .map(|set| walk::matches_globs(set, &rel))
                .unwrap_or(false)
        {
            continue;
        }
        let base = repo
            .find_object(oid)
            .with_context(|| format!("cannot read Git blob for deleted file `{rel}`"))?;
        if base.data.len() as u64 > cli.max_file_size || filter::looks_binary(&base.data) {
            continue;
        }
        deleted.push(rel);
    }
    deleted.sort();

    Ok(Selection {
        candidates: changed,
        kinds,
        deleted,
    })
}

fn is_ignored_deleted(repo: &gix::Repository, workdir: &Path, path: &Path) -> Result<bool> {
    let mut ignored = false;
    let (global, global_error) = GitignoreBuilder::new(workdir).build_global();
    if let Some(error) = global_error {
        return Err(anyhow!("cannot read global ignore rules: {error}"));
    }
    apply_ignore_match(&global, path, &mut ignored);

    let exclude = repo.path().join("info").join("exclude");
    if exclude.is_file() {
        let mut builder = GitignoreBuilder::new(workdir);
        if let Some(error) = builder.add(&exclude) {
            return Err(anyhow!("cannot read `{}`: {error}", exclude.display()));
        }
        let matcher = builder
            .build()
            .context("cannot build Git exclude matcher")?;
        apply_ignore_match(&matcher, path, &mut ignored);
    }

    let relative = path
        .strip_prefix(workdir)
        .context("deleted path is outside the Git working tree")?;
    let mut directories = vec![workdir.to_path_buf()];
    let mut current = workdir.to_path_buf();
    for component in relative.parent().into_iter().flat_map(Path::components) {
        current.push(component.as_os_str());
        directories.push(current.clone());
    }
    for directory in directories {
        let mut builder = GitignoreBuilder::new(&directory);
        for name in [".gitignore", ".contextignore", ".ignore"] {
            let ignore_file = directory.join(name);
            if ignore_file.is_file() {
                if let Some(error) = builder.add(&ignore_file) {
                    return Err(anyhow!("cannot read `{}`: {error}", ignore_file.display()));
                }
            }
        }
        let matcher = builder
            .build()
            .with_context(|| format!("cannot build ignore matcher in `{}`", directory.display()))?;
        apply_ignore_match(&matcher, path, &mut ignored);
    }
    Ok(ignored)
}

fn apply_ignore_match(matcher: &Gitignore, path: &Path, ignored: &mut bool) {
    match matcher.matched_path_or_any_parents(path, false) {
        Match::Ignore(_) => *ignored = true,
        Match::Whitelist(_) => *ignored = false,
        Match::None => {}
    }
}

fn git_relative_path(workdir: &Path, cwd: &Path, path: &Path) -> Result<String> {
    let absolute = absolute_lexical(cwd, path);
    let parent = absolute
        .parent()
        .ok_or_else(|| anyhow!("path `{}` has no parent directory", path.display()))?;
    let parent = std::fs::canonicalize(parent)
        .with_context(|| format!("cannot resolve parent of `{}`", path.display()))?;
    let absolute = parent.join(
        absolute
            .file_name()
            .ok_or_else(|| anyhow!("path `{}` has no filename", path.display()))?,
    );
    let workdir = std::fs::canonicalize(workdir).context("cannot resolve the Git working tree")?;
    let relative = absolute
        .strip_prefix(&workdir)
        .map_err(|_| anyhow!("path `{}` is outside the Git working tree", path.display()))?;
    Ok(relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/"))
}

fn normalize_crlf(bytes: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            index += 1;
        }
        normalized.push(bytes[index]);
        index += 1;
    }
    normalized
}

fn absolute_lexical(cwd: &Path, path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                output.pop();
            }
            other => output.push(other.as_os_str()),
        }
    }
    output
}

fn in_scope(path: &Path, scopes: &[PathBuf]) -> bool {
    scopes.iter().any(|scope| path.starts_with(scope))
}

fn display_path(cwd: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(cwd).unwrap_or(path);
    relative.to_string_lossy().replace('\\', "/")
}

fn path_is_hidden(path: &str) -> bool {
    path.split('/').any(|part| part.starts_with('.'))
}

fn one_line(value: &str) -> String {
    value.replace(['\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_crlf_but_not_bare_cr() {
        assert_eq!(normalize_crlf(b"a\r\nb\r\n"), b"a\nb\n");
        assert_eq!(normalize_crlf(b"a\rb"), b"a\rb");
    }

    #[cfg(windows)]
    #[test]
    fn normalizes_relative_scopes_lexically() {
        let cwd = Path::new("C:\\repo\\nested");
        assert_eq!(
            absolute_lexical(cwd, Path::new("..\\src")),
            PathBuf::from("C:\\repo\\src")
        );
    }

    #[cfg(unix)]
    #[test]
    fn normalizes_relative_scopes_lexically() {
        let cwd = Path::new("/repo/nested");
        assert_eq!(
            absolute_lexical(cwd, Path::new("../src")),
            PathBuf::from("/repo/src")
        );
    }
}
