//! Priority scoring, budget enforcement and the truncation planner.
//!
//! Priority is a pure function of a file's path, size and first few lines, so
//! it is fully unit-testable and, crucially, deterministic: the same tree
//! always truncates to the same payload.

use crate::tokens;

/// Separation between whole tiers. A tier is worth more than any achievable
/// depth or size advantage, so a tier-2 file can never overtake a tier-1 file.
const TIER_STRIDE: i64 = 10_000_000;
/// Separation for one level of directory depth.
const DEPTH_STRIDE: i64 = 10_000;
/// Largest size term, in KiB, so size never crosses a depth step.
const SIZE_CAP_KIB: i64 = 9_999;
/// Deepest directory depth that earns a depth bonus.
const DEPTH_CAP: usize = 99;

/// Files at least this large are treated as "very large" and drop to the
/// lowest priority tier. Independent of `--max-file-size`, which decides what
/// is included at all.
pub const VERY_LARGE: u64 = 64 * 1024;

/// Top-level manifests, alongside `README*`.
pub const MANIFESTS: &[&str] = &["Cargo.toml", "package.json", "pyproject.toml", "go.mod"];

/// Filename stems that count as entry points.
pub const ENTRY_POINTS: &[&str] = &["main", "index", "lib", "app", "mod", "__init__"];

/// Path components that mark tests / specs.
pub const TEST_DIRS: &[&str] = &["test", "tests", "spec", "__tests__"];

/// Path components that mark docs, examples and fixtures.
pub const LOW_DIRS: &[&str] = &["doc", "docs", "example", "examples", "fixture", "fixtures"];

/// Path components that mark generated or vendored trees.
pub const GENERATED_DIRS: &[&str] = &["gen", "generated", "vendor", "dist", "build"];

/// Markers in the first few lines that identify generated files.
pub const GENERATED_MARKERS: &[&str] = &["DO NOT EDIT", "@generated"];

/// Everything the priority function is allowed to look at.
#[derive(Debug, Clone, Copy)]
pub struct PriorityInput<'a> {
    /// Path relative to the working directory, with `/` separators.
    pub rel: &'a str,
    /// True when the path was named on the command line rather than discovered
    /// by walking a directory.
    pub explicit: bool,
    /// Directory depth: 0 for a file at the root.
    pub depth: usize,
    /// Size in bytes.
    pub size: u64,
    /// First few lines of the file, for generated-file detection.
    pub head: &'a str,
}

/// The basename of a `/`-separated relative path.
pub fn basename(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// True when any *directory* component of `rel` is in `dirs`
/// (case-insensitive). The filename itself is not considered.
fn is_within(rel: &str, dirs: &[&str]) -> bool {
    let mut comps: Vec<&str> = rel.split('/').collect();
    comps.pop();
    comps.iter().any(|c| {
        let lower = c.to_ascii_lowercase();
        dirs.iter().any(|d| *d == lower)
    })
}

/// True for `*_test.*`, `*.test.*`, `*.spec.*` and `test_*` filenames.
fn is_test_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("test_")
        || lower.contains("_test.")
        || lower.contains(".test.")
        || lower.contains(".spec.")
}

/// True for `main.rs`, `index.tsx`, `lib.py`, `mod.rs`, `__init__.py`, ...
fn is_entry_point(name: &str) -> bool {
    let stem = match name.split_once('.') {
        Some((stem, _)) => stem,
        None => name,
    };
    ENTRY_POINTS.contains(&stem)
}

fn is_generated(input: &PriorityInput) -> bool {
    if is_within(input.rel, GENERATED_DIRS) {
        return true;
    }
    GENERATED_MARKERS.iter().any(|m| input.head.contains(m))
}

/// Priority tier, 1 (highest) to 5 (lowest).
fn tier_of(input: &PriorityInput) -> i64 {
    let name = basename(input.rel);
    // 1. README, top-level manifests, and anything named on the command line.
    if input.explicit || name.to_ascii_lowercase().starts_with("readme") {
        return 1;
    }
    if input.depth == 0 && MANIFESTS.contains(&name) {
        return 1;
    }
    // 5. Generated-looking and very large files go first.
    if is_generated(input) || input.size >= VERY_LARGE {
        return 5;
    }
    // 4. Tests, docs, examples, fixtures.
    if is_within(input.rel, TEST_DIRS) || is_within(input.rel, LOW_DIRS) || is_test_name(name) {
        return 4;
    }
    // 2. Entry points and shallow files.
    if is_entry_point(name) || input.depth <= 1 {
        return 2;
    }
    // 3. Everything else.
    3
}

/// Priority score: higher means "keep longer".
///
/// Within a tier, shallower files win, and at equal depth smaller files win.
pub fn priority_score(input: &PriorityInput) -> i64 {
    let tier = tier_of(input);
    let depth = input.depth.min(DEPTH_CAP) as i64;
    let kib = (input.size / 1024).min(SIZE_CAP_KIB as u64) as i64;
    // A path the user named on the command line is the very last thing
    // --truncate gives up, even against a root README. The bonus is half a
    // tier, so it cannot lift a file over a tier above it.
    let explicit = if input.explicit { TIER_STRIDE / 2 } else { 0 };
    explicit + (6 - tier) * TIER_STRIDE + (DEPTH_CAP as i64 - depth) * DEPTH_STRIDE - kib
}

// ---------------------------------------------------------------------------
// Per-directory token breakdown
// ---------------------------------------------------------------------------

/// A directory in the breakdown tree. `tokens` covers the directory's own
/// files plus everything nested below it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DirNode {
    pub name: String,
    pub tokens: u64,
    pub dirs: Vec<DirNode>,
    pub files: Vec<(String, u64)>,
}

impl DirNode {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            tokens: 0,
            dirs: Vec::new(),
            files: Vec::new(),
        }
    }
}

/// Build the breakdown tree from `(relative path, tokens)` pairs.
pub fn build_breakdown<'a, I>(entries: I) -> DirNode
where
    I: IntoIterator<Item = (&'a str, u64)>,
{
    let mut root = DirNode::new(".");
    for (rel, tokens) in entries {
        let comps: Vec<&str> = rel.split('/').filter(|c| !c.is_empty()).collect();
        if comps.is_empty() {
            continue;
        }
        let mut cur = &mut root;
        cur.tokens += tokens;
        for comp in &comps[..comps.len() - 1] {
            let idx = match cur.dirs.iter().position(|d| d.name == *comp) {
                Some(i) => i,
                None => {
                    cur.dirs.push(DirNode::new(*comp));
                    cur.dirs.len() - 1
                }
            };
            cur = &mut cur.dirs[idx];
            cur.tokens += tokens;
        }
        cur.files.push((comps[comps.len() - 1].to_string(), tokens));
    }
    root
}

// ---------------------------------------------------------------------------
// Truncation planning
// ---------------------------------------------------------------------------

/// One file's contribution to the payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contribution {
    /// Path relative to the working directory.
    pub rel: String,
    /// Bytes this file's content block occupies (0 for an empty file, which is
    /// listed in the tree but emits no block).
    pub block_bytes: u64,
    /// Priority score; higher is kept longer.
    pub priority: i64,
}

/// The outcome of planning: which files to omit and what that leaves behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Parallel to the input contributions; `true` means "omit this file".
    pub omit: Vec<bool>,
    pub dropped: usize,
    pub tokens_saved: u64,
    pub total_bytes: u64,
    pub total_tokens: u64,
    /// False when even dropping everything would not fit.
    pub fits: bool,
}

impl Plan {
    /// Number of files that survived planning. Only the tests ask for this;
    /// the binary reports the tree size and the drop count instead.
    #[cfg(test)]
    pub fn kept(&self) -> usize {
        self.omit.iter().filter(|o| !**o).count()
    }
}

/// Chooses whole files to drop so the payload fits inside `limit` tokens.
///
/// Accounting is exact rather than approximate. The payload is a plain
/// concatenation, so its size is
///
/// ```text
/// fixed_bytes + sum(kept block_bytes) + dropped * annotation_bytes
/// ```
///
/// and the only rounded quantity is the final `ceil(bytes / ratio)`. No file is
/// ever partially emitted, and no re-rendering is needed to decide.
#[derive(Debug, Clone, Copy)]
pub struct Planner<'a> {
    pub contributions: &'a [Contribution],
    /// Payload bytes with no content blocks at all: format wrapper plus one
    /// plain tree line per file.
    pub fixed_bytes: u64,
    /// Bytes a single omission annotation adds to a tree line. Zero when the
    /// tree is disabled, because then there is nothing to annotate.
    pub annotation_bytes: u64,
    pub ratio: f64,
    /// Effective limit: the budget after the safety margin.
    pub limit: u64,
}

impl<'a> Planner<'a> {
    /// Payload bytes for a given omission set.
    pub fn total_bytes(&self, omit: &[bool]) -> u64 {
        let mut bytes = self.fixed_bytes;
        for (i, c) in self.contributions.iter().enumerate() {
            if omit[i] {
                bytes += self.annotation_bytes;
            } else {
                bytes += c.block_bytes;
            }
        }
        bytes
    }

    /// The smallest payload this planner can produce: every file omitted.
    /// Only the tests ask for this directly; `plan()` reports the same figure
    /// through `Plan::total_bytes` when nothing fits.
    #[cfg(test)]
    pub fn min_bytes(&self) -> u64 {
        self.fixed_bytes + self.annotation_bytes * self.contributions.len() as u64
    }

    fn tokens_for(&self, bytes: u64) -> u64 {
        tokens::estimate_bytes(bytes, self.ratio)
    }

    /// Drop order: lowest priority first; at equal priority the largest file
    /// first (it frees the most budget), then by path for determinism.
    pub fn drop_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.contributions.len()).collect();
        order.sort_by(|&a, &b| {
            let (ca, cb) = (&self.contributions[a], &self.contributions[b]);
            ca.priority
                .cmp(&cb.priority)
                .then_with(|| cb.block_bytes.cmp(&ca.block_bytes))
                .then_with(|| ca.rel.cmp(&cb.rel))
        });
        order
    }

    /// Plan the smallest set of drops that fits, using the drop order above.
    pub fn plan(&self) -> Plan {
        let mut omit = vec![false; self.contributions.len()];
        let before = self.tokens_for(self.total_bytes(&omit));
        let mut bytes = self.total_bytes(&omit);
        let mut dropped = 0usize;

        for idx in self.drop_order() {
            if self.tokens_for(bytes) <= self.limit {
                break;
            }
            omit[idx] = true;
            dropped += 1;
            bytes = bytes
                .saturating_sub(self.contributions[idx].block_bytes)
                .saturating_add(self.annotation_bytes);
        }

        let total_tokens = self.tokens_for(bytes);
        Plan {
            omit,
            dropped,
            tokens_saved: before.saturating_sub(total_tokens),
            total_bytes: bytes,
            total_tokens,
            fits: total_tokens <= self.limit,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(rel: &'a str, depth: usize, size: u64, head: &'a str) -> PriorityInput<'a> {
        PriorityInput {
            rel,
            explicit: false,
            depth,
            size,
            head,
        }
    }

    #[test]
    fn readme_beats_everything_ordinary() {
        let readme = priority_score(&input("README.md", 0, 5000, ""));
        let source = priority_score(&input("src/a.rs", 1, 10, ""));
        let deep_source = priority_score(&input("src/deep/nest/a.rs", 3, 10, ""));
        assert!(readme > source, "README should outrank source");
        assert!(readme > deep_source);
    }

    #[test]
    fn explicit_paths_outrank_discovered_ones() {
        let explicit = PriorityInput {
            explicit: true,
            ..input("some/deep/test/thing_test.rs", 4, 9000, "")
        };
        let discovered = priority_score(&input("README.md", 0, 10, ""));
        assert!(
            priority_score(&explicit) > discovered,
            "an explicitly named path is the highest priority"
        );
    }

    #[test]
    fn top_level_manifests_outrank_nested_ones() {
        let top = priority_score(&input("Cargo.toml", 0, 900, ""));
        let nested = priority_score(&input("crates/x/Cargo.toml", 2, 900, ""));
        assert!(top > nested);
    }

    #[test]
    fn entry_points_and_shallow_files_beat_deep_general_files() {
        let entry = priority_score(&input("src/main.rs", 1, 400, ""));
        let deep = priority_score(&input("src/a/b/c/util.rs", 4, 400, ""));
        let shallow = priority_score(&input("a.rs", 0, 400, ""));
        let general = priority_score(&input("src/a/b/util.rs", 3, 400, ""));
        assert!(entry > deep);
        assert!(shallow > general);
    }

    #[test]
    fn tests_docs_and_fixtures_rank_below_general_source() {
        let general = priority_score(&input("src/util.rs", 2, 400, ""));
        for low in [
            "src/tests/util.rs",
            "tests/integration.rs",
            "spec/a_spec.js",
            "src/util_test.rs",
            "src/util.test.ts",
            "src/util.spec.ts",
            "docs/guide.md",
            "examples/demo.rs",
            "fixtures/data.txt",
        ] {
            let depth = low.matches('/').count();
            assert!(
                priority_score(&input(low, depth, 400, "")) < general,
                "{low} should rank below general source"
            );
        }
    }

    #[test]
    fn generated_and_very_large_files_rank_lowest() {
        let test = priority_score(&input("tests/a.rs", 1, 400, ""));
        for generated in [
            "dist/bundle.js",
            "vendor/lib/a.rs",
            "src/generated/schema.rs",
            "build/out.rs",
        ] {
            let depth = generated.matches('/').count();
            assert!(
                priority_score(&input(generated, depth, 400, "")) < test,
                "{generated} should rank below tests"
            );
        }
        let marked = priority_score(&input("src/thing.rs", 1, 100, "// DO NOT EDIT\n"));
        let normal = priority_score(&input("src/other.rs", 1, 100, "// hello\n"));
        assert!(marked < normal, "a `DO NOT EDIT` header lowers priority");

        let huge = priority_score(&input("src/big.rs", 1, VERY_LARGE, ""));
        let small = priority_score(&input("src/small.rs", 1, 100, ""));
        assert!(huge < small);
    }

    #[test]
    fn at_equal_tier_smaller_files_are_kept_longer() {
        let small = priority_score(&input("src/a.rs", 2, 100, ""));
        let large = priority_score(&input("src/b.rs", 2, 4_000, ""));
        assert!(small > large);
    }

    #[test]
    fn shallower_files_win_within_a_tier() {
        let shallow = priority_score(&input("src/a.rs", 1, 100, ""));
        let deep = priority_score(&input("src/a/b.rs", 2, 100, ""));
        assert!(shallow > deep);
    }

    #[test]
    fn tiers_never_overlap() {
        // The lowest possible tier-1 score still beats the highest tier-2 one.
        let worst_tier1 = {
            let mut i = input("README.md", DEPTH_CAP, VERY_LARGE - 1, "");
            i.rel = "README.md";
            priority_score(&i)
        };
        let best_tier2 = priority_score(&input("main.rs", 0, 0, ""));
        assert!(worst_tier1 > best_tier2, "{worst_tier1} vs {best_tier2}");
    }

    #[test]
    fn entry_point_detection_covers_the_documented_names() {
        for name in [
            "main.rs",
            "index.tsx",
            "lib.py",
            "app.go",
            "mod.rs",
            "__init__.py",
            "main",
        ] {
            assert!(is_entry_point(name), "{name} should be an entry point");
        }
        for name in ["mainline.rs", "libs.rs", "indexer.py", "appendix.md"] {
            assert!(!is_entry_point(name), "{name} should not be an entry point");
        }
    }

    #[test]
    fn breakdown_nests_and_totals() {
        let root = build_breakdown([
            ("a.rs", 10u64),
            ("src/main.rs", 20),
            ("src/lib.rs", 5),
            ("src/deep/x.rs", 7),
        ]);
        assert_eq!(root.tokens, 42);
        assert_eq!(root.files, vec![("a.rs".to_string(), 10)]);
        let src = root.dirs.iter().find(|d| d.name == "src").expect("src");
        assert_eq!(src.tokens, 32);
        assert_eq!(src.files.len(), 2);
        let deep = src.dirs.iter().find(|d| d.name == "deep").expect("deep");
        assert_eq!(deep.tokens, 7);
        assert_eq!(deep.files, vec![("x.rs".to_string(), 7)]);
    }

    fn contributions() -> Vec<Contribution> {
        vec![
            Contribution {
                rel: "README.md".into(),
                block_bytes: 1000,
                priority: 50_000_000,
            },
            Contribution {
                rel: "src/main.rs".into(),
                block_bytes: 500,
                priority: 40_000_000,
            },
            Contribution {
                rel: "tests/x_test.rs".into(),
                block_bytes: 400,
                priority: 20_000_000,
            },
        ]
    }

    fn planner<'a>(c: &'a [Contribution], limit: u64) -> Planner<'a> {
        Planner {
            contributions: c,
            fixed_bytes: 100,
            annotation_bytes: 20,
            ratio: 1.0,
            limit,
        }
    }

    #[test]
    fn planner_keeps_everything_when_it_already_fits() {
        let c = contributions();
        let p = planner(&c, 10_000).plan();
        assert_eq!(p.dropped, 0);
        assert!(p.fits);
        assert_eq!(p.total_bytes, 100 + 1000 + 500 + 400);
        assert_eq!(p.kept(), 3);
        assert_eq!(p.tokens_saved, 0);
    }

    #[test]
    fn planner_drops_lowest_priority_first_and_keeps_readme() {
        let c = contributions();
        let plan = planner(&c, 1200).plan();
        assert!(plan.fits);
        // tests/x_test.rs goes first, then src/main.rs, never README.md.
        assert_eq!(plan.omit, vec![false, true, true]);
        assert_eq!(plan.dropped, 2);
        assert_eq!(plan.total_bytes, 100 + 1000 + 20 + 20);
        assert!(plan.tokens_saved > 0);
    }

    #[test]
    fn planner_never_drops_a_file_it_does_not_need_to() {
        let c = contributions();
        // Just barely fits after dropping only the test file.
        let plan = planner(&c, 1700).plan();
        assert!(plan.fits);
        assert_eq!(plan.omit, vec![false, false, true]);
        assert_eq!(plan.dropped, 1);
    }

    #[test]
    fn planner_reports_failure_when_even_the_tree_is_too_big() {
        let c = contributions();
        let plan = planner(&c, 50).plan();
        assert!(!plan.fits);
        assert_eq!(plan.dropped, 3);
        assert_eq!(plan.total_bytes, planner(&c, 50).min_bytes());
        assert_eq!(plan.total_bytes, 100 + 3 * 20);
    }

    #[test]
    fn planner_drop_order_is_deterministic() {
        let c = contributions();
        let first = planner(&c, 1).drop_order();
        for _ in 0..20 {
            assert_eq!(planner(&c, 1).drop_order(), first);
        }
        // Lowest priority last in the keep order => first to be dropped.
        assert_eq!(first[0], 2);
    }

    #[test]
    fn planner_handles_exact_and_adjacent_budget_boundaries() {
        let c = vec![Contribution {
            rel: "only.txt".into(),
            block_bytes: 100,
            priority: 1,
        }];
        for (limit, expected_omit) in [(111, false), (110, false), (109, true)] {
            let p = Planner {
                contributions: &c,
                fixed_bytes: 10,
                annotation_bytes: 1,
                ratio: 1.0,
                limit,
            };
            let plan = p.plan();
            assert!(plan.fits, "limit={limit}");
            assert_eq!(plan.omit, vec![expected_omit], "limit={limit}");
            assert!(plan.total_tokens <= limit, "limit={limit}: {plan:?}");
        }
    }

    #[test]
    fn planner_ties_are_deterministic_and_explicit_files_drop_last() {
        let tied = vec![
            Contribution {
                rel: "b.txt".into(),
                block_bytes: 50,
                priority: 10,
            },
            Contribution {
                rel: "a.txt".into(),
                block_bytes: 50,
                priority: 10,
            },
        ];
        let tie_plan = Planner {
            contributions: &tied,
            fixed_bytes: 10,
            annotation_bytes: 1,
            ratio: 1.0,
            limit: 61,
        }
        .plan();
        assert_eq!(tie_plan.omit, vec![false, true]);
        for _ in 0..20 {
            assert_eq!(
                Planner {
                    contributions: &tied,
                    fixed_bytes: 10,
                    annotation_bytes: 1,
                    ratio: 1.0,
                    limit: 61,
                }
                .plan(),
                tie_plan
            );
        }

        let explicit = vec![
            Contribution {
                rel: "discovered.txt".into(),
                block_bytes: 50,
                priority: 10,
            },
            Contribution {
                rel: "named.txt".into(),
                block_bytes: 50,
                priority: 20,
            },
        ];
        let plan = Planner {
            contributions: &explicit,
            fixed_bytes: 10,
            annotation_bytes: 1,
            ratio: 1.0,
            limit: 61,
        }
        .plan();
        assert_eq!(plan.omit, vec![true, false]);
        assert!(plan.total_tokens <= 61);
    }

    #[test]
    fn randomized_plans_never_exceed_a_fitting_effective_limit() {
        let mut state = 0x7d31_4a5bu64;
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            state
        };
        for case in 0..500 {
            let count = (next() % 12 + 1) as usize;
            let contributions: Vec<_> = (0..count)
                .map(|index| Contribution {
                    rel: format!("{case:03}/{index:02}.txt"),
                    block_bytes: next() % 20_000,
                    priority: (next() % 100_000) as i64,
                })
                .collect();
            let fixed_bytes = next() % 2_000;
            let annotation_bytes = next() % 24;
            let ratio = [0.25, 1.0, 3.5, 17.0, 1024.0][(next() % 5) as usize];
            let limit = next() % 20_000;
            let p = Planner {
                contributions: &contributions,
                fixed_bytes,
                annotation_bytes,
                ratio,
                limit,
            };
            let plan = p.plan();
            assert_eq!(p.plan(), plan, "case={case} is not deterministic");
            assert_eq!(p.total_bytes(&plan.omit), plan.total_bytes, "case={case}");
            assert_eq!(
                plan.total_tokens,
                tokens::estimate_bytes(plan.total_bytes, ratio),
                "case={case}"
            );
            if plan.fits {
                assert!(plan.total_tokens <= limit, "case={case}: {plan:?}");
            } else {
                assert!(plan.omit.iter().all(|omitted| *omitted), "case={case}");
                assert!(plan.total_tokens > limit, "case={case}: {plan:?}");
            }
        }
    }

    #[test]
    fn planner_without_a_tree_does_not_charge_for_annotations() {
        let c = contributions();
        let p = Planner {
            contributions: &c,
            fixed_bytes: 100,
            annotation_bytes: 0,
            ratio: 1.0,
            limit: 1100,
        };
        let plan = p.plan();
        assert!(plan.fits);
        assert_eq!(plan.omit, vec![false, true, true]);
        assert_eq!(plan.total_bytes, 1100);
    }

    #[test]
    fn empty_files_cost_nothing_but_are_still_listed() {
        let c = vec![
            Contribution {
                rel: "a.rs".into(),
                block_bytes: 900,
                priority: 10,
            },
            Contribution {
                rel: "empty.rs".into(),
                block_bytes: 0,
                priority: 5,
            },
        ];
        // An empty file has no content block at all: payload = 100 + 900.
        let plan = planner(&c, 1000).plan();
        assert!(plan.fits);
        assert_eq!(plan.dropped, 0, "nothing needs dropping");
        assert_eq!(plan.total_bytes, 1000);

        // When a drop *is* needed, the lowest-priority file goes first even
        // though dropping it costs annotation bytes rather than saving any.
        let plan = planner(&c, 950).plan();
        assert!(plan.fits);
        assert_eq!(plan.omit, vec![true, true]);
        assert_eq!(plan.total_bytes, 100 + 20 + 20);
    }
}
