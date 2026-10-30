//! Stderr reporting: summaries, warnings, colour and the token breakdown tree.
//!
//! stdout is reserved for the payload, so every human-facing message in `ctx`
//! goes through this module, and therefore to stderr.

use std::io::{self, IsTerminal, Write};

use crate::budget::DirNode;
use crate::filter::SkipCounts;
use crate::tokens;

/// Terminal width the breakdown is laid out for.
const LAYOUT_WIDTH: usize = 79;

/// Entries shown per directory before collapsing into `... and K more`.
pub const TOP_N: usize = 8;

/// Insert thousands separators: `38212` -> `38,212`.
pub fn commas(n: u64) -> String {
    let s = n.to_string();
    let len = s.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Shorten `s` to at most `max` characters (by `char` count), marking the cut.
fn truncate(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max || max == 0 {
        return s.to_string();
    }
    let keep = max.saturating_sub(3);
    let mut out: String = s.chars().take(keep).collect();
    out.push_str("...");
    out
}

#[derive(Clone, Copy, Debug)]
pub struct Ui {
    quiet: bool,
    color: bool,
}

impl Ui {
    /// Colour is on only when stderr is a terminal, `--no-color` was not
    /// passed and `NO_COLOR` is unset.
    pub fn new(quiet: bool, no_color: bool) -> Self {
        let color =
            !no_color && std::env::var_os("NO_COLOR").is_none() && io::stderr().is_terminal();
        Self { quiet, color }
    }

    fn paint(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("\u{1b}[{code}m{s}\u{1b}[0m")
        } else {
            s.to_string()
        }
    }

    fn line(&self, s: &str) {
        let mut err = io::stderr();
        let _ = writeln!(err, "{s}");
    }

    /// Warning. Suppressed by `--quiet`.
    pub fn warn(&self, msg: &str) {
        if !self.quiet {
            let prefix = self.paint("33", "ctx: warning:");
            self.line(&format!("{prefix} {msg}"));
        }
    }

    /// Informational line. Suppressed by `--quiet`.
    pub fn note(&self, msg: &str) {
        if !self.quiet {
            let prefix = self.paint("36", "ctx:");
            self.line(&format!("{prefix} {msg}"));
        }
    }

    /// Hint shown under an error. Suppressed by `--quiet`.
    pub fn hint(&self, msg: &str) {
        if !self.quiet {
            let prefix = self.paint("2", "hint:");
            self.line(&format!("  {prefix} {msg}"));
        }
    }

    /// Error. Never suppressed.
    pub fn error(&self, msg: &str) {
        let prefix = self.paint("31", "ctx: error:");
        self.line(&format!("{prefix} {msg}"));
    }

    /// `ctx: 41 files, 38,212 tokens (est.) / 50,000 budget`
    pub fn summary(&self, files: usize, tokens: u64, budget: Option<u64>) {
        if self.quiet {
            return;
        }
        let prefix = self.paint("36", "ctx:");
        let files_label = if files == 1 { "file" } else { "files" };
        let counts = tokens::format_tokens(tokens);
        match budget {
            Some(budget) => self.line(&format!(
                "{prefix} {files} {files_label}, {counts} / {} budget",
                commas(budget)
            )),
            None => self.line(&format!("{prefix} {files} {files_label}, {counts}")),
        }
    }

    /// `ctx: skipped: 3 binary, 1 too large, 2 lockfiles, 1 secret`
    pub fn skipped(&self, counts: &SkipCounts) {
        if self.quiet {
            return;
        }
        let parts = counts.describe();
        if parts.is_empty() {
            return;
        }
        let prefix = self.paint("36", "ctx:");
        self.line(&format!("{prefix} skipped: {}", parts.join(", ")));
    }

    /// Render the breakdown tree as text (pure, so it can be unit-tested).
    ///
    /// Entries are sorted by size, descending, and each directory shows only
    /// its [`TOP_N`] largest entries. When `mark_largest` is set, the single
    /// biggest entry at each level is flagged.
    pub fn render_breakdown(root: &DirNode, top_n: usize, mark_largest: bool) -> String {
        let mut out = String::new();
        emit_row(&mut out, 0, &root.name, root.tokens, false);
        render_children(root, 1, top_n, mark_largest, &mut out);
        out
    }

    /// Print a breakdown tree to stderr.
    pub fn breakdown(&self, root: &DirNode, top_n: usize, mark_largest: bool) {
        let text = Self::render_breakdown(root, top_n, mark_largest);
        let mut err = io::stderr();
        let _ = write!(err, "{text}");
    }
}

fn render_children(node: &DirNode, depth: usize, top_n: usize, mark: bool, out: &mut String) {
    let mut rows: Vec<(String, u64, Option<&DirNode>)> = Vec::new();
    for dir in &node.dirs {
        rows.push((format!("{}/", dir.name), dir.tokens, Some(dir)));
    }
    for (name, tokens) in &node.files {
        rows.push((name.clone(), *tokens, None));
    }
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let shown = rows.len().min(top_n);
    for (i, (label, tokens, dir)) in rows.iter().enumerate().take(shown) {
        emit_row(out, depth, label, *tokens, mark && i == 0);
        if let Some(child) = dir {
            render_children(child, depth + 1, top_n, mark, out);
        }
    }
    if rows.len() > shown {
        let rest: u64 = rows[shown..].iter().map(|r| r.1).sum();
        let hidden = rows.len() - shown;
        emit_row(out, depth, &format!("... and {hidden} more"), rest, false);
    }
}

fn emit_row(out: &mut String, depth: usize, label: &str, tokens: u64, largest: bool) {
    let indent = "  ".repeat(depth);
    let count = commas(tokens);
    let marker = if largest { "  <-- largest" } else { "" };
    let fixed = indent.chars().count() + count.chars().count() + marker.chars().count();
    let label = truncate(label, LAYOUT_WIDTH.saturating_sub(fixed + 1));
    let used = indent.chars().count()
        + label.chars().count()
        + count.chars().count()
        + marker.chars().count();
    let pad = " ".repeat(LAYOUT_WIDTH.saturating_sub(used));
    out.push_str(&format!("{indent}{label}{pad}{count}{marker}\n"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::build_breakdown;

    #[test]
    fn commas_groups_thousands() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(7), "7");
        assert_eq!(commas(999), "999");
        assert_eq!(commas(1_000), "1,000");
        assert_eq!(commas(38_212), "38,212");
        assert_eq!(commas(1_234_567), "1,234,567");
    }

    #[test]
    fn truncate_marks_the_cut() {
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("abcdefg", 5), "ab...");
        assert_eq!(truncate("abcdefg", 3), "...");
    }

    #[test]
    fn breakdown_rows_fit_in_80_columns() {
        let root = build_breakdown([
            (
                "a/very/deeply/nested/path/with/a/long/name/file_with_a_long_name.rs",
                1u64,
            ),
            ("b.rs", 2),
        ]);
        for line in Ui::render_breakdown(&root, TOP_N, false).lines() {
            assert!(
                line.chars().count() <= LAYOUT_WIDTH,
                "line too long ({}): {line}",
                line.chars().count()
            );
        }
    }

    #[test]
    fn breakdown_sorts_by_size_descending_and_collapses_the_tail() {
        let entries: Vec<(String, u64)> = (0..12)
            .map(|i| (format!("src/f{i:02}.rs"), (i + 1) as u64 * 10))
            .collect();
        let root = build_breakdown(entries.iter().map(|(a, b)| (a.as_str(), *b)));
        let text = Ui::render_breakdown(&root, TOP_N, false);
        assert!(
            text.contains("... and 4 more"),
            "expected a collapse line:\n{text}"
        );
        // The single largest file is listed before the collapsed remainder.
        let big = text.find("f11.rs").expect("largest file listed");
        let collapse = text.find("... and").expect("collapse line");
        assert!(big < collapse);
        // The 4 smallest files (10+20+30+40 = 100 tokens) are collapsed.
        let collapse_line = text
            .lines()
            .find(|line| line.contains("... and"))
            .expect("collapse line");
        assert!(
            collapse_line.trim_end().ends_with("100"),
            "collapsed total should be summed: {collapse_line:?}"
        );
    }

    #[test]
    fn breakdown_marks_only_the_largest_entry_per_level() {
        let root = build_breakdown([("a.rs", 10u64), ("b.rs", 90)]);
        let marked = Ui::render_breakdown(&root, TOP_N, true);
        assert_eq!(marked.matches("<-- largest").count(), 1);
        let unmarked = Ui::render_breakdown(&root, TOP_N, false);
        assert_eq!(unmarked.matches("<-- largest").count(), 0);
    }

    #[test]
    fn breakdown_collapses_a_deep_tail_into_one_row_per_level() {
        // 12 files spread over two directories; only the top entries of each
        // directory should be listed.
        let mut entries: Vec<(String, u64)> = Vec::new();
        for i in 0..12 {
            entries.push((format!("src/f{i:02}.rs"), (i + 1) as u64 * 10));
        }
        for i in 0..3 {
            entries.push((format!("docs/d{i}.md"), (i + 1) as u64));
        }
        let root = build_breakdown(entries.iter().map(|(a, b)| (a.as_str(), *b)));
        let text = Ui::render_breakdown(&root, TOP_N, false);
        assert!(text.contains("src/"), "{text}");
        assert!(text.contains("docs/"), "{text}");
        // src holds 780 tokens, docs holds 6; both are listed at the root.
        assert!(text.contains("780"), "{text}");
    }

    #[test]
    fn breakdown_prints_the_root_total() {
        let root = build_breakdown([("a.rs", 1234u64)]);
        let text = Ui::render_breakdown(&root, TOP_N, false);
        assert!(text.starts_with(".\n") || text.starts_with("."), "{text}");
        assert!(text.contains("1,234"), "{text}");
    }
}
