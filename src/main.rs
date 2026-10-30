#![forbid(unsafe_code)]
//! `ctx` — pack a directory's structure and file contents into one
//! budget-checked, LLM-ready payload.
//!
//! stdout carries the payload and nothing else. Every stat, warning, progress
//! message and error goes to stderr, on every code path, so that
//! `ctx > context.xml` always yields a clean file.

mod budget;
mod cli;
mod filter;
mod output;
mod render;
mod tokens;
mod ui;
mod walk;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;

use crate::budget::{Contribution, Planner};
use crate::cli::{Cli, BUDGET_SAFETY_MARGIN};
use crate::filter::{LoadOutcome, SkipCounts};
use crate::output::{CopyOutcome, StdoutOutcome};
use crate::render::{Payload, RenderedFile};
use crate::ui::{commas, Ui, TOP_N};
use crate::walk::{Candidate, WalkCfg};

/// Documented exit codes (also listed in `--help`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exit {
    Success = 0,
    Runtime = 1,
    Usage = 2,
    OverBudget = 3,
    Nothing = 4,
}

#[derive(Default)]
struct PhaseTimings {
    walk: Duration,
    filter_read: Duration,
    sort: Duration,
    render: Duration,
    estimate: Duration,
    write: Duration,
}

impl PhaseTimings {
    fn report_if_enabled(&self) {
        if std::env::var_os("CTX_TIMING").as_deref() != Some(std::ffi::OsStr::new("1")) {
            return;
        }
        eprintln!(
            "ctx timing: walk={:.3}ms filter/read={:.3}ms sort={:.3}ms render={:.3}ms estimate={:.3}ms write={:.3}ms",
            self.walk.as_secs_f64() * 1000.0,
            self.filter_read.as_secs_f64() * 1000.0,
            self.sort.as_secs_f64() * 1000.0,
            self.render.as_secs_f64() * 1000.0,
            self.estimate.as_secs_f64() * 1000.0,
            self.write.as_secs_f64() * 1000.0,
        );
    }
}

fn main() {
    // clap already writes usage errors to stderr with status 2, and
    // --help/--version to stdout with status 0.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => err.exit(),
    };
    let ui = Ui::new(cli.quiet, cli.no_color);
    if let Some(notice) = tokens::exact_notice() {
        ui.warn(notice);
    }

    let code = match run(&cli, &ui) {
        Ok(code) => code,
        Err(err) => {
            // `{:#}` prints the whole anyhow chain on one line, and never a
            // stack trace.
            ui.error(&format!("{err:#}"));
            Exit::Runtime
        }
    };
    std::process::exit(code as i32);
}

fn run(cli: &Cli, ui: &Ui) -> Result<Exit> {
    let mut timings = PhaseTimings::default();
    let cwd = std::env::current_dir().context("cannot determine the current directory")?;
    let paths: Vec<PathBuf> = if cli.paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        cli.paths.clone()
    };

    for path in &paths {
        if !path.exists() {
            ui.error(&format!("no such file or directory: `{}`", path.display()));
            ui.hint("positional arguments must be existing files or directories");
            return Ok(Exit::Runtime);
        }
    }

    let include = match walk::build_globs(&cli.include, "include") {
        Ok(set) => set,
        Err(message) => {
            ui.error(&message);
            return Ok(Exit::Usage);
        }
    };
    let exclude = match walk::build_globs(&cli.exclude, "exclude") {
        Ok(set) => set,
        Err(message) => {
            ui.error(&message);
            return Ok(Exit::Usage);
        }
    };

    let cfg = WalkCfg {
        paths: &paths,
        hidden: cli.hidden,
        no_ignore: cli.no_ignore,
        follow_symlinks: cli.follow_symlinks,
        include: include.as_ref(),
        exclude: exclude.as_ref(),
        cwd: &cwd,
    };
    let phase_started = Instant::now();
    let mut walked = walk::collect(&cfg)?;
    timings.walk = phase_started.elapsed();
    let phase_started = Instant::now();
    walked.candidates.sort_by(|a, b| a.rel.cmp(&b.rel));
    timings.sort = phase_started.elapsed();
    let mut saw_loop = false;
    for warning in &walked.warnings {
        // The walker reports symlink loops here rather than hanging.
        saw_loop |= warning.to_ascii_lowercase().contains("loop");
        ui.warn(warning);
    }
    if saw_loop {
        ui.hint("symlink loops are skipped; the walk continued normally");
    }

    // --- name- and size-based skip rules -----------------------------------
    let phase_started = Instant::now();
    let mut counts = SkipCounts::default();
    let mut kept: Vec<Candidate> = Vec::with_capacity(walked.candidates.len());
    for candidate in walked.candidates {
        if candidate.size > cli.max_file_size {
            counts.too_large += 1;
            continue;
        }
        let name = filter::file_name(&candidate.abs);
        if !cli.include_lockfiles && filter::is_lockfile_name(name) {
            counts.lockfile += 1;
            continue;
        }
        if !cli.include_secrets && filter::is_secret_name(name) {
            counts.secret += 1;
            continue;
        }
        if filter::is_junk_name(name) {
            // Build junk (`*.min.js`, `*.map`, `*.snap`, ...) is skipped unless
            // an --include glob asks for it explicitly.
            let explicitly_included = include
                .as_ref()
                .map(|set| walk::matches_globs(set, &candidate.rel))
                .unwrap_or(false);
            if !explicitly_included {
                counts.junk += 1;
                continue;
            }
        }
        kept.push(candidate);
    }

    // --- read, validate and render -----------------------------------------
    let to_read: Vec<PathBuf> = kept.iter().map(|c| c.abs.clone()).collect();
    let loaded = filter::load_many(&to_read);
    let mut loaded_files: Vec<(Candidate, String)> = Vec::with_capacity(kept.len());
    for (candidate, outcome) in kept.into_iter().zip(loaded) {
        match outcome {
            LoadOutcome::Text(text) => loaded_files.push((candidate, text)),
            LoadOutcome::Binary => counts.binary += 1,
            LoadOutcome::Failed(message) => {
                ui.warn(&format!("skipping `{}`: {message}", candidate.rel));
            }
        }
    }
    timings.filter_read = phase_started.elapsed();

    let phase_started = Instant::now();
    let mut files: Vec<RenderedFile> = Vec::with_capacity(loaded_files.len());
    for (candidate, text) in loaded_files {
        let head = filter::first_lines(&text, 5);
        let priority = budget::priority_score(&budget::PriorityInput {
            rel: &candidate.rel,
            explicit: candidate.explicit,
            depth: candidate.depth,
            size: candidate.size,
            head: &head,
        });
        let block = render::render_block(
            cli.format,
            &candidate.rel,
            &render::normalize_content(&text),
        );
        files.push(RenderedFile {
            rel: candidate.rel.clone(),
            block_bytes: block.len() as u64,
            block,
            priority,
            depth: candidate.depth,
        });
    }
    timings.render = phase_started.elapsed();

    if files.is_empty() {
        ui.error("nothing to output: no files matched");
        ui.hint("check --include/--exclude and ignore files, or pass a different path");
        return Ok(Exit::Nothing);
    }

    // --- budget ------------------------------------------------------------
    let phase_started = Instant::now();
    let ratio = cli.ratio;
    let annotation_bytes = if cli.no_tree {
        0
    } else {
        tokens::annotation_bytes()
    };
    let contributions: Vec<Contribution> = files
        .iter()
        .map(|file| Contribution {
            rel: file.rel.clone(),
            // With --tree-only no content is emitted, so no bytes are charged
            // for it; the tree itself is accounted in `fixed_bytes`.
            block_bytes: if cli.tree_only { 0 } else { file.block_bytes },
            priority: file.priority,
        })
        .collect();
    let fixed_bytes = render::fixed_bytes(cli.format, cli.no_tree, &files);
    let full_bytes = fixed_bytes + contributions.iter().map(|c| c.block_bytes).sum::<u64>();
    let full_tokens = tokens::estimate_bytes(full_bytes, ratio);

    let mut omit = vec![false; files.len()];
    let mut truncation: Option<(usize, u64)> = None;

    if let Some(budget) = cli.budget {
        let limit = (budget as f64 * BUDGET_SAFETY_MARGIN).floor() as u64;
        if full_tokens > limit {
            if !cli.truncate {
                report_over_budget(ui, &files, ratio, cli.tree_only, full_tokens, budget, limit);
                return Ok(Exit::OverBudget);
            }
            let planner = Planner {
                contributions: &contributions,
                fixed_bytes,
                annotation_bytes,
                ratio,
                limit,
            };
            let plan = planner.plan();
            if !plan.fits {
                ui.error(&format!(
                    "even the tree alone is {} tokens (est.), over the {} token limit (--budget {} with a 5% safety margin)",
                    commas(plan.total_tokens),
                    commas(limit),
                    commas(budget)
                ));
                ui.hint("every file was dropped, so --truncate cannot help; raise --budget or narrow the input");
                return Ok(Exit::OverBudget);
            }
            omit = plan.omit;
            truncation = Some((plan.dropped, plan.tokens_saved));
        }
    }

    if cli.stats {
        let entries: Vec<(&str, u64)> = files
            .iter()
            .map(|file| {
                let bytes = if cli.tree_only { 0 } else { file.block_bytes };
                (file.rel.as_str(), tokens::estimate_bytes(bytes, ratio))
            })
            .collect();
        ui.note("token breakdown by directory (est.):");
        ui.breakdown(&budget::build_breakdown(entries), TOP_N, true);
    }
    timings.estimate = phase_started.elapsed();

    // --- render and emit ---------------------------------------------------
    let render_started = Instant::now();
    let payload = render::render_payload(&Payload {
        format: cli.format,
        no_tree: cli.no_tree,
        tree_only: cli.tree_only,
        files: &files,
        omit: &omit,
    });
    timings.render += render_started.elapsed();
    let estimate_started = Instant::now();
    let payload_tokens = tokens::estimate_payload(&payload, ratio);
    timings.estimate += estimate_started.elapsed();
    debug_assert_eq!(
        payload.len() as u64,
        fixed_bytes
            + contributions
                .iter()
                .enumerate()
                .map(|(i, c)| if omit[i] {
                    annotation_bytes
                } else {
                    c.block_bytes
                })
                .sum::<u64>(),
        "byte accounting drifted from the rendered payload"
    );
    let phase_started = Instant::now();
    if let Some(path) = &cli.output {
        output::write_file(path, &payload)?;
        ui.note(&format!(
            "wrote {} to `{}`",
            tokens::format_tokens(payload_tokens),
            path.display()
        ));
    }

    if cli.copy {
        #[cfg(target_os = "linux")]
        if std::env::var_os("DISPLAY").is_some() {
            ui.note(
                "waiting for X11 clipboard ownership transfer; without a clipboard manager, --copy may stay active until another owner replaces the contents"
            );
        }
        match output::copy_to_clipboard(&payload) {
            Ok(CopyOutcome::Copied) => ui.note(&format!(
                "copied to clipboard ({} tokens est.)",
                commas(payload_tokens)
            )),
            Err(err) => {
                ui.error(&format!("cannot use the clipboard: {err:#}"));
                ui.hint("use `--output <FILE>` instead, or let ctx write to stdout and pipe it to a clipboard tool such as `wl-copy`");
                return Ok(Exit::Runtime);
            }
        }
    }

    if cli.output.is_none() && !cli.copy {
        if std::io::stdout().is_terminal() {
            ui.note("writing the payload to stdout; use --copy to put it on the clipboard");
        }
        match output::write_stdout(&payload)? {
            StdoutOutcome::Written => {}
            // The reader went away. Leave quietly with success.
            StdoutOutcome::ClosedEarly => {
                timings.write = phase_started.elapsed();
                timings.report_if_enabled();
                return Ok(Exit::Success);
            }
        }
    }
    timings.write = phase_started.elapsed();

    ui.summary(files.len(), payload_tokens, cli.budget);
    ui.skipped(&counts);
    if let Some((dropped, saved)) = truncation {
        ui.note(&format!(
            "truncated: dropped {dropped} files, saved {} tokens (est.)",
            commas(saved)
        ));
    }
    timings.report_if_enabled();

    Ok(Exit::Success)
}

/// Explain an over-budget payload: the total, the budget, and where the tokens
/// actually are. Nothing is written to stdout on this path.
fn report_over_budget(
    ui: &Ui,
    files: &[RenderedFile],
    ratio: f64,
    tree_only: bool,
    total: u64,
    budget: u64,
    limit: u64,
) {
    ui.error(&format!(
        "payload would be {} tokens (est.), over the {} token limit (--budget {} with a 5% safety margin)",
        commas(total),
        commas(limit),
        commas(budget)
    ));
    ui.note("largest contributors (est.):");
    let entries: Vec<(&str, u64)> = files
        .iter()
        .map(|file| {
            let bytes = if tree_only { 0 } else { file.block_bytes };
            (file.rel.as_str(), tokens::estimate_bytes(bytes, ratio))
        })
        .collect();
    ui.breakdown(&budget::build_breakdown(entries), TOP_N, true);
    ui.hint("re-run with --truncate to drop whole files until it fits, or raise --budget");
}
