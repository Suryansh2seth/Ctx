//! Command-line surface: the flag definitions plus the size/budget suffix
//! parsers.
//!
//! Parsing rules that are easy to get wrong are implemented as small pure
//! functions and unit-tested directly, rather than only through clap.

use std::path::PathBuf;

use clap::{Parser, ValueEnum};

/// Default bytes-per-token divisor for the heuristic estimator.
pub const DEFAULT_RATIO: f64 = 3.5;

/// Default `--max-file-size`.
pub const DEFAULT_MAX_FILE_SIZE: u64 = 256 * 1024;

/// Fraction of `--budget` that is actually enforced (the 5% safety margin).
pub const BUDGET_SAFETY_MARGIN: f64 = 0.95;

/// Multiplier for a `k` suffix. Binary, so `--budget 8k` is 8192 tokens, which
/// lines up with how LLM context windows are actually sized (8192, 32768,
/// 131072, 1048576).
const KIB: u64 = 1024;
const MIB: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// Explicitly tagged `<tree>` / `<file path="...">` blocks (default).
    Xml,
    /// `## Tree` section, then `## path` headings with fenced code blocks.
    Md,
    /// Bare path list, then `===== path =====` separators.
    Plain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SecretScan {
    Redact,
    Skip,
    Off,
}

const AFTER_HELP: &str = "\
Exit codes:
  0  success
  1  runtime or I/O error
  2  usage error (bad flags)
  3  over budget without --truncate
  4  nothing to output (no files matched)

Token counts are estimates, never exact: tokens = ceil(bytes / --ratio), and
the default ratio is 3.5 bytes per token. A 5% safety margin is applied when
comparing against --budget, so `--budget 128k` (131,072) refuses to exceed
124,518 estimated tokens.

Sizes and budgets accept `k` and `m` suffixes, which are binary (k = 1024,
m = 1048576): `--max-file-size 512k`, `--budget 8k` (8192 tokens).

The payload goes to stdout. Every diagnostic goes to stderr, so
`ctx > context.xml` always produces a clean, redirectable file.

On Linux/X11, `--copy` stays alive until another clipboard owner replaces the
selection, so the content remains available to paste. Unless `--quiet` is
passed, a stderr notice is printed before the handoff. Without a clipboard
manager or later clipboard replacement, the command can remain active
indefinitely. Pressing Ctrl-C while ctx waits on Linux can lose the copied
content unless a clipboard manager has already taken it; use `--output <FILE>`
or a pipe to avoid this. If no display is available, use `--output <FILE>`, or
omit `--copy` and pipe stdout to a clipboard tool yourself.";

#[derive(Debug, Parser)]
#[command(
    name = "ctx",
    version,
    about = "Pack a directory's structure and file contents into one budget-checked, LLM-ready payload.",
    after_help = AFTER_HELP,
    disable_help_subcommand = true,
    max_term_width = 100
)]
pub struct Cli {
    /// Files or directories to pack (default: current directory)
    #[arg(value_name = "PATHS")]
    pub paths: Vec<PathBuf>,

    /// Maximum estimated tokens for the payload (e.g. 8k, 32k, 128k, 1m)
    #[arg(short = 'b', long, value_name = "TOKENS", value_parser = parse_budget)]
    pub budget: Option<u64>,

    /// Drop whole files, lowest priority first, until the payload fits
    #[arg(short = 't', long)]
    pub truncate: bool,

    /// Output format
    #[arg(short = 'f', long, value_name = "FMT", value_enum, default_value_t = Format::Xml)]
    pub format: Format,

    /// Send the payload to the clipboard instead of stdout
    #[arg(short = 'c', long)]
    pub copy: bool,

    /// Write the payload to a file instead of stdout
    #[arg(short = 'o', long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Only include files matching this glob (repeatable)
    #[arg(short = 'i', long, value_name = "GLOB")]
    pub include: Vec<String>,

    /// Exclude files matching this glob (repeatable, applied on top of ignore files)
    #[arg(short = 'e', long, value_name = "GLOB")]
    pub exclude: Vec<String>,

    /// Skip files larger than this (k/m suffixes allowed)
    #[arg(long, value_name = "SIZE", default_value_t = DEFAULT_MAX_FILE_SIZE, value_parser = parse_size)]
    pub max_file_size: u64,

    /// Include hidden files and directories (still respects ignore files)
    #[arg(long)]
    pub hidden: bool,

    /// Do not read .gitignore/.contextignore/.ignore or global git excludes
    #[arg(long)]
    pub no_ignore: bool,

    /// Follow symlinks (default: do not); loops are detected and reported
    #[arg(long)]
    pub follow_symlinks: bool,

    /// Only include files changed since this Git revision
    #[arg(long, value_name = "REV")]
    pub since: Option<String>,

    /// Include lockfiles (skipped by default)
    #[arg(long)]
    pub include_lockfiles: bool,

    /// Disable the default secret-file denylist
    #[arg(long)]
    pub include_secrets: bool,

    /// Scan file contents for likely secrets: redact (default), skip, or off
    #[arg(long, value_enum, default_value_t = SecretScan::Redact)]
    pub secret_scan: SecretScan,

    /// Omit the tree section
    #[arg(long)]
    pub no_tree: bool,

    /// Output only the tree (no file contents)
    #[arg(long)]
    pub tree_only: bool,

    /// Print the per-directory token breakdown to stderr even when under budget
    #[arg(long)]
    pub stats: bool,

    /// Bytes per token for the heuristic estimator
    #[arg(long, value_name = "FLOAT", default_value_t = DEFAULT_RATIO, value_parser = parse_ratio)]
    pub ratio: f64,

    /// Suppress all non-error stderr output
    #[arg(short = 'q', long)]
    pub quiet: bool,

    /// Disable color on stderr (also honours the NO_COLOR env var)
    #[arg(long)]
    pub no_color: bool,
}

/// Parse a byte size with an optional binary `k`/`m` suffix.
pub fn parse_size(s: &str) -> Result<u64, String> {
    parse_scaled(s, "size")
}

/// Parse a token budget with an optional binary `k`/`m` suffix.
pub fn parse_budget(s: &str) -> Result<u64, String> {
    parse_scaled(s, "token budget")
}

fn parse_scaled(s: &str, what: &str) -> Result<u64, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "expected a {what} such as 256k or 1m, got an empty value"
        ));
    }
    let lower = trimmed.to_ascii_lowercase();
    let (digits, mult) = if let Some(d) = lower.strip_suffix('k') {
        (d, KIB)
    } else if let Some(d) = lower.strip_suffix('m') {
        (d, MIB)
    } else {
        (lower.as_str(), 1)
    };
    let digits = digits.trim();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "`{s}` is not a valid {what}; use a whole number of bytes, optionally with a k/m suffix (e.g. 256k, 1m)"
        ));
    }
    let n: u64 = digits
        .parse()
        .map_err(|_| format!("`{s}` is too large for a {what}"))?;
    n.checked_mul(mult)
        .ok_or_else(|| format!("`{s}` is too large for a {what}"))
}

/// Bytes-per-token ratio: finite and strictly positive.
pub fn parse_ratio(s: &str) -> Result<f64, String> {
    let v: f64 = s.trim().parse().map_err(|_| {
        format!("`{s}` is not a number; expected a bytes-per-token ratio such as 3.5")
    })?;
    if !v.is_finite() || v <= 0.0 {
        return Err(format!("ratio must be greater than 0, got `{s}`"));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn sizes_accept_k_and_m_suffixes() {
        assert_eq!(parse_size("256k").unwrap(), 262_144);
        assert_eq!(parse_size("512K").unwrap(), 524_288);
        assert_eq!(parse_size("1m").unwrap(), 1_048_576);
        assert_eq!(parse_size("1M").unwrap(), 1_048_576);
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("0").unwrap(), 0);
        assert_eq!(parse_size("  2k  ").unwrap(), 2048);
    }

    #[test]
    fn budgets_accept_the_documented_suffixes() {
        assert_eq!(parse_budget("8k").unwrap(), 8_192);
        assert_eq!(parse_budget("32k").unwrap(), 32_768);
        assert_eq!(parse_budget("128k").unwrap(), 131_072);
        assert_eq!(parse_budget("1m").unwrap(), 1_048_576);
        assert_eq!(parse_budget("50000").unwrap(), 50_000);
    }

    #[test]
    fn bad_sizes_are_rejected_with_a_helpful_message() {
        for bad in ["", "   ", "abc", "1g", "1.5k", "-1", "k", "1kk", "0x10"] {
            let err = parse_size(bad).unwrap_err();
            assert!(!err.is_empty(), "empty message for {bad:?}");
        }
    }

    #[test]
    fn oversized_values_are_rejected_rather_than_wrapping() {
        assert!(parse_size("99999999999999999999m").is_err());
    }

    #[test]
    fn ratios_must_be_positive_and_finite() {
        assert_eq!(parse_ratio("3.5").unwrap(), 3.5);
        assert_eq!(parse_ratio("1").unwrap(), 1.0);
        for bad in ["0", "-1", "abc", "inf", "nan", ""] {
            assert!(parse_ratio(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn default_ratio_and_margin_match_the_documented_values() {
        assert_eq!(DEFAULT_RATIO, 3.5);
        assert_eq!(DEFAULT_MAX_FILE_SIZE, 262_144);
        assert!((BUDGET_SAFETY_MARGIN - 0.95).abs() < f64::EPSILON);
    }

    #[test]
    fn parses_a_realistic_invocation() {
        let cli = Cli::try_parse_from([
            "ctx",
            "src",
            "--budget",
            "32k",
            "-t",
            "-f",
            "md",
            "--hidden",
            "-i",
            "*.rs",
            "-i",
            "*.toml",
            "-e",
            "target/**",
            "--stats",
        ])
        .expect("should parse");
        assert_eq!(cli.paths, vec![PathBuf::from("src")]);
        assert_eq!(cli.budget, Some(32_768));
        assert!(cli.truncate);
        assert_eq!(cli.format, Format::Md);
        assert!(cli.hidden);
        assert_eq!(cli.include, vec!["*.rs".to_string(), "*.toml".to_string()]);
        assert_eq!(cli.exclude, vec!["target/**".to_string()]);
        assert!(cli.stats);
        assert!(!cli.quiet);
    }

    #[test]
    fn defaults_are_applied_when_nothing_is_given() {
        let cli = Cli::try_parse_from(["ctx"]).expect("should parse");
        assert!(cli.paths.is_empty());
        assert_eq!(cli.budget, None);
        assert_eq!(cli.format, Format::Xml);
        assert_eq!(cli.max_file_size, DEFAULT_MAX_FILE_SIZE);
        assert_eq!(cli.ratio, DEFAULT_RATIO);
        assert!(!cli.copy && !cli.output.is_some() && !cli.tree_only && !cli.no_tree);
    }

    #[test]
    fn unknown_flags_and_bad_values_fail_to_parse() {
        assert!(Cli::try_parse_from(["ctx", "--nope"]).is_err());
        assert!(Cli::try_parse_from(["ctx", "--budget", "lots"]).is_err());
        assert!(Cli::try_parse_from(["ctx", "--ratio", "0"]).is_err());
        assert!(Cli::try_parse_from(["ctx", "--format", "html"]).is_err());
    }

    #[test]
    fn help_and_version_are_available_under_both_spellings() {
        for flag in ["-h", "--help"] {
            let err = Cli::try_parse_from(["ctx", flag]).unwrap_err();
            assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp, "{flag}");
        }
        for flag in ["-V", "--version"] {
            let err = Cli::try_parse_from(["ctx", flag]).unwrap_err();
            assert_eq!(err.kind(), clap::error::ErrorKind::DisplayVersion, "{flag}");
        }
    }

    #[test]
    fn help_documents_exit_codes_and_the_safety_margin() {
        let help = <Cli as clap::CommandFactory>::command()
            .render_long_help()
            .to_string();
        for needle in [
            "Exit codes:",
            "3  over budget",
            "5% safety margin",
            "estimates",
            "--since <REV>",
            "Pressing Ctrl-C",
            "clipboard manager has already taken it",
        ] {
            assert!(help.contains(needle), "help is missing {needle:?}");
        }
    }
}
