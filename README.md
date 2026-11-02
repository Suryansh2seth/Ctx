# ctx

Pack a directory's structure and file contents into **one budget-checked,
LLM-ready text payload** — from a single native binary, with no subprocesses
and no network access.

```
ctx                      # current directory  -> stdout
ctx src tests            # several paths
ctx -b 32k --copy        # fit inside 32k tokens, straight to the clipboard
ctx -b 128k -t -o ctx.xml   # truncate to fit, write to a file
```

`ctx` is the "paste my repo into a model" step, done properly: deterministic,
ignore-aware, secret-aware, and honest about how big the result is.

[Project repository](https://github.com/Suryansh2seth/Ctx)

---

## The problem

Getting a codebase into an LLM usually means one of:

- `cat`-ing files together, which drags in `.git`, `node_modules`, lockfiles,
  binaries, build output and a stray `.env` with real credentials;
- pasting files by hand, and losing track of what was included;
- discovering only after the request fails that the payload was 3× the context
  window.

`ctx` does the boring, error-prone parts: walk the tree once, honour the ignore
rules the project already has, skip the junk and the secrets, wrap every file in
an explicit tag, estimate the size, and either fit inside your budget or tell
you exactly what to drop.

## Captured demo

```console
$ ctx --stats --no-color
<context>
<tree>
Cargo.toml
README.md
src/main.rs
</tree>
<file path="Cargo.toml">
[package]
name = 'sample'
version = '0.1.0'

[dependencies]
</file>
<file path="README.md">
# Sample
A tiny Rust project.
</file>
<file path="src/main.rs">
fn main() {
}
</file>
</context>
ctx: token breakdown by directory (est.):
.                                                                            59
  Cargo.toml                                                    27  <-- largest
  README.md                                                                  18
  src/                                                                       14
    main.rs                                                     14  <-- largest
ctx: 3 files, 78 tokens (est.)
```

Captured from the existing release executable (`ctx 0.1.0`) on Windows using a
three-file sample tree. The 59-token breakdown covers file blocks; the 78-token
total also includes the rendered tree and payload structure.

## Install

```console
# from crates.io once published (requires Rust 1.88 or newer; installs `ctx`)
cargo install ctxdump

# or build a small release binary
cargo build --release --locked      # target/release/ctx
```

Prebuilt binaries for `x86_64`/`aarch64` Linux, `x86_64`/`aarch64` macOS and
`x86_64` Windows are attached to every tagged GitHub release, produced by
`.github/workflows/release.yml` (see
[the releases page](https://github.com/Suryansh2seth/Ctx/releases)).

## Usage

```
ctx [OPTIONS] [PATHS]...
```

All inputs are positional; every option is optional. With no `PATHS`, the
current directory is used. `PATHS` may mix files and directories, and files
reached through more than one path are included once.

### Flags

| Flag | Default | Meaning |
| --- | --- | --- |
| `-b, --budget <TOKENS>` | *none* | Maximum **estimated** tokens. Accepts `8k`, `32k`, `128k`, `1m`. |
| `-t, --truncate` | off | If over budget, drop whole files, lowest priority first, until it fits. Without this, exceeding the budget is an error. |
| `-f, --format <FMT>` | `xml` | `xml`, `md` or `plain`. |
| `-c, --copy` | off | Send the payload to the clipboard instead of stdout. |
| `-o, --output <FILE>` | *none* | Write the payload to a file instead of stdout. May be combined with `--copy`. |
| `-i, --include <GLOB>` | *none* | Only include files matching this glob. Repeatable. Acts as an allowlist applied after ignore rules, and is also what opts `*.min.js`/`*.map`/`*.snap` back in. |
| `-e, --exclude <GLOB>` | *none* | Exclude files matching this glob. Repeatable. Applied on top of ignore files. |
| `--max-file-size <SIZE>` | `256k` | Skip files larger than this. Accepts `k`/`m`. |
| `--hidden` | off | Include hidden files and directories (still respects ignore files; `.git/` is always skipped). |
| `--no-ignore` | off | Do not read `.gitignore`, `.contextignore`, `.ignore`, `.git/info/exclude` or the global git ignore. |
| `--follow-symlinks` | off | Follow symlinks; loops are detected and reported as warnings, never hung on. |
| `--since <REV>` | *none* | Include changed, added and deleted paths relative to a Git revision; unchanged files are omitted. |
| `--include-lockfiles` | off | Include lockfiles. |
| `--include-secrets` | off | Disable the default secret-file denylist. |
| `--secret-scan <MODE>` | `redact` | Scan eligible text contents for likely secrets: replace matches, skip matched files, or turn scanning off. |
| `--no-tree` | off | Omit the tree section. |
| `--tree-only` | off | Output only the tree, with no file contents. |
| `--stats` | off | Print the per-directory token breakdown to stderr even when under budget. |
| `--ratio <FLOAT>` | `3.5` | Bytes per token for the heuristic estimator. |
| `-q, --quiet` | off | Suppress all non-error stderr output. |
| `--no-color` | off | Disable colour on stderr (also honours `NO_COLOR`; colour is off automatically when stderr is not a TTY). |
| `-h, --help` / `-V, --version` | | Both spellings work. |

On Linux/X11, `--copy` stays active until another clipboard owner replaces the
selection, keeping the payload available to paste. Unless `--quiet` is passed,
`ctx` prints a stderr notice before waiting. Without a clipboard manager or a
later clipboard replacement, the command can remain active indefinitely.
Pressing Ctrl-C while `ctx` waits on Linux can lose the copied content unless a
clipboard manager has already taken it; use `--output <FILE>` or a pipe to
avoid this. If no display is available, use `--output <FILE>`, or omit
`--copy` and pipe stdout to a clipboard tool yourself.

### Changed files with `--since`

`--since <REV>` compares working-tree file contents with the selected Git
revision (a branch, tag, commit, or relative form such as `HEAD~3`). Staged,
unstaged, and untracked non-ignored files are considered. Deleted files are
shown in the tree with `  [deleted]` and have no content block; unchanged files
are omitted. Existing ignore, glob, size, binary, lockfile, secret, and budget
filters still apply. The selected revision is included in the XML root
attribute, or in a heading/first line for Markdown/plain output. SHA-256 Git
repositories are not supported. This feature uses in-process Git object access;
it does not invoke Git or access the network.

The `--since` option is enabled in the default build. A binary built with
`--no-default-features` reports that Git support is required if the option is
used.

### Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success |
| `1` | Runtime or I/O error (missing path, unwritable output, unusable clipboard) |
| `2` | Usage error (bad flag or value) |
| `3` | Over budget without `--truncate` |
| `4` | Nothing to output (no files matched) |

### Streams

**stdout carries the payload and nothing else.** Every stat, warning, progress
message and error goes to stderr, on every code path — including error paths, so
`ctx > context.xml` never leaves a half-written file with diagnostics in it.
Closing stdout early (`ctx | head -c 10`) is treated as success and exits
quietly.

In Windows PowerShell, `>` can re-encode native command output as UTF-16. Use
`--output <FILE>` when you need the exact UTF-8 payload bytes.

## Output formats

### `xml` (default)

Each file gets an explicit path attribute, so the boundaries are unambiguous
even for a model that only skims.

```xml
<context>
<tree>
Cargo.toml
src/main.rs
</tree>
<file path="Cargo.toml">
[package]
name = "demo"
</file>
<file path="src/main.rs">
fn main() {
    println!("hi");
}
</file>
</context>
```

Ordinary code is **not** XML-escaped — escaping `<`, `>` and `&` would both
waste tokens and corrupt the code. Only the two sequences that could actually
close a block, `</file>` and `</context>`, are escaped (to `&lt;/file>` and
`&lt;/context>`), and only in that exact form. A file containing a literal
`</file>` therefore cannot break the structure.

### `md`

A `## Tree` section in a fenced block, then one `## path/to/file` heading per
file, each followed by a fenced code block with a language hint derived from the
extension. The fence is always longer than the longest run of backticks inside
the file, so content containing ``` ``` ``` cannot terminate its own block.

````markdown
## Tree

```text
Cargo.toml
src/main.rs
```

## src/main.rs

```rust
fn main() {
    println!("hi");
}
```
````

### `plain`

```
Cargo.toml
src/main.rs

===== Cargo.toml =====
[package]
name = "demo"

===== src/main.rs =====
fn main() {
    println!("hi");
}
```

In every format the tree comes first (unless `--no-tree`), contents follow
(unless `--tree-only`), paths always use `/` as the separator, no box-drawing or
indentation art is used, empty files are listed in the tree but emit no content
block, and every file's content ends with exactly one trailing newline.

## What gets skipped

A file is left out when it is:

1. **Binary** — a NUL byte in the first 8 KiB, or more than ~1% replacement
   characters after a lossy UTF-8 decode. Detected from content, not extension.
2. **Too large** — over `--max-file-size` (default 256k).
3. **A lockfile** — any filename ending in `.lock`, plus `package-lock.json`,
   `npm-shrinkwrap.json`, `pnpm-lock.yaml`, `go.sum`, `packages.lock.json`,
   `gradle.lockfile`, `Package.resolved`, and `bun.lockb`. Override with
   `--include-lockfiles`. The rule is a filename suffix match, so
   `clock.lock.rs` is an ordinary source file and is not skipped.
4. **Build/vendor junk** — `*.min.js`, `*.min.css`, `*.map`, `*.snap`. An
   `--include` glob that matches the file opts it back in.
5. **A likely secret filename** — `.env`, `.env.*` (except `.env.example` and
   `.env.sample`), `*.pem`, `*.key`, `id_rsa*`, `id_ed25519*`, `*.p12`, `*.pfx`,
   `credentials.json`, `.npmrc`, `.pypirc`, `.netrc`. Override with
   `--include-secrets`. Files excluded by filename are not read or scanned.

After those exclusions, eligible text files are scanned for likely secret
contents by default. `--secret-scan redact` replaces matched values with
`[REDACTED:<kind>]`; `--secret-scan skip` excludes the whole matching file and
counts it as a skipped secret; `--secret-scan off` disables content scanning.
`--include-secrets` only overrides the filename denylist; it does not disable
content scanning. Matches are summarized on stderr by kind and file path, never
by secret value.

`.contextignore` is also honored, including nested files and negation patterns.
It uses the same gitignore syntax as `.gitignore`, and is never included in the
payload. `--no-ignore` disables `.contextignore` along with the other ignore
sources.

Skipped files are summarised on stderr by reason:

```console
ctx: 41 files, 38,212 tokens (est.) / 50,000 budget
ctx: skipped: 3 binary, 1 too large, 2 lockfiles, 1 secret
```

`.env` is hidden and is also denied by filename as a likely secret by default.
To include it, pass both `--hidden` and `--include-secrets`. Content-scan skip
mode also contributes to the `skipped: N secrets` summary.

### Content scanning

The built-in content scan recognizes PEM private-key blocks; AWS access-key
IDs; GitHub, Slack, Google, Stripe, Anthropic and OpenAI-style tokens; JWTs;
and high-entropy quoted values assigned to names such as `api_key`, `secret`,
`token` or `password`. For generic assignments, the current threshold is
Shannon entropy greater than 4.2 bits per character for values at least 20
ASCII characters long. Ordinary hexadecimal commit SHAs have at most 4 bits
per character and are therefore below that threshold; common base64 strings
may also remain below it. The generic rule only examines values assigned to
secret-like names; ordinary fields such as `blob` are not candidates. This
heuristic can still produce false positives
for unusual high-entropy values and false negatives for unrecognized formats,
short values or values with low entropy. The generic assignment rule only
recognizes quoted values that begin and end on the same line; it does not parse
structured formats, so unquoted YAML/TOML/`.env` assignments and values split
across lines are not detected by that rule. Provider-specific tokens and PEM
blocks can still be recognized independently of assignment syntax.

This scan is a best-effort heuristic, not a guarantee or a substitute for a
dedicated secret scanner. If the presence of a secret is uncertain, use an
external scanner before sharing the payload.

## Budget and truncation

`--budget` sets a ceiling on the **estimated** size of the whole payload — tree,
tags and contents together, not just the file bodies.

- **Without `--truncate`**, exceeding the budget is an error: nothing is written
  to stdout, the clipboard or the output file, and stderr gets the total, the
  budget, and a per-directory breakdown sorted by size with the largest entries
  marked. Exit code 3.
- **With `--truncate`**, whole files are dropped until the payload fits. A file
  is never cut mid-content; dropped files stay listed in the tree so the model
  still knows they exist:

  ```
  src/big.rs  [omitted: budget]
  ```

  A summary of how many files were dropped and how many tokens that saved is
  printed to stderr.
- If even the tree alone exceeds the budget, `ctx` says so and exits 3.

### Priority: what gets dropped last

Higher priority is kept longer, and the ordering is a pure, unit-tested function
of path, size and the first few lines:

1. **Highest** — `README*`, top-level manifests (`Cargo.toml`, `package.json`,
   `pyproject.toml`, `go.mod`), and files you named explicitly on the command
   line.
2. **High** — entry points (`main.*`, `index.*`, `lib.*`, `app.*`, `mod.rs`,
   `__init__.py`) and shallow files.
3. **Medium** — general source files; at equal depth, smaller files first.
4. **Low** — tests (`test`/`tests`/`spec`/`__tests__` paths, `*_test.*`,
   `*.test.*`, `*.spec.*`), docs, examples, fixtures.
5. **Lowest** — generated-looking files (paths containing
   `generated`/`gen`/`vendor`/`dist`/`build`, or a `DO NOT EDIT`/`@generated`
   marker in the first 5 lines) and very large files.

Truncation is deterministic: the same tree always produces byte-for-byte the
same payload, regardless of how many threads the walker used.

## Estimates, not exact

`ctx` ships a **heuristic** estimator by default:

```
tokens = ceil(bytes / ratio)        # ratio defaults to 3.5 bytes per token
```

3.5 is deliberately conservative for source code, but it is still an estimate.
Real tokenizers vary by content: dense code and non-ASCII text cost more tokens
per byte, prose and whitespace cost fewer. Every number `ctx` prints is labelled
`(est.)` for that reason, and the estimate is applied to the **final rendered
payload** — tags, tree and all.

**A 5% safety margin is applied when enforcing `--budget`.** The effective limit
is `budget × 0.95`, so `--budget 128k` (131,072) refuses to exceed 124,518
estimated tokens. This is headroom for the estimator being wrong in the
optimistic direction.

Tune the ratio for your material with `--ratio`: lower it (for example `--ratio
3.0`) for dense code, raise it for prose.

There is an optional `exact` feature intended for a real BPE tokenizer. **It is
a stub today**: the feature gate, the call site and the fallback all exist and
are tested, but the estimator returns "unknown" and every build uses the
heuristic. See `src/tokens.rs`.

## Security

- **No subprocesses, ever.** `ctx` never runs `git`, `xclip`, `wl-copy`,
  `rg`, a shell, or anything else. Traversal uses the `ignore` crate in-process,
  and the clipboard uses `arboard`, which talks to the platform clipboard API
  directly. The crate is `#![forbid(unsafe_code)]`.
- **No network.** `ctx` makes no network calls of any kind.
- **No persistent state, no config files.** Flags are the whole interface.
- **Secrets are denied by default** (see the denylist above), and a skipped
- **Secret filenames are denied by default**, before their contents are read.
  Other eligible text content is scanned heuristically and redacted by default.
  Diagnostics include counts, kinds and paths but never matched values.
  `--include-secrets` only overrides filename filtering. This heuristic is not
  guaranteed and does not replace a dedicated secret scanner.
- Ignore rules already in the project are respected, including nested
  `.gitignore` and `.contextignore` files, `.ignore`, `.git/info/exclude`,
  global git excludes and negation patterns.

## Comparison with Repomix

`ctx` is a native Rust command-line tool that emits `xml`, `md`, or `plain`
payloads to stdout, a file, or the clipboard, with a byte-based token estimate
and optional whole-file truncation. Repomix's current README describes packing
repositories for LLMs and lists token counts, configurable include/exclude
patterns, `.gitignore`/`.ignore`/`.repomixignore` support, Secretlint-based
secret filtering, code compression, and remote-repository input. See the
[Repomix README](https://github.com/yamadashy/repomix/blob/main/README.md) for
its current details; this comparison makes no claim about relative speed or
runtime requirements.

## Benchmarks

`benches/bench.ps1` (Windows) and `benches/bench.sh` (Linux/macOS) time
whole-process runs on either a synthetic tree or a real checkout. They are
scripts rather than a `criterion` benchmark on purpose: what matters here is a
complete run over a real filesystem — walk, ignore matching, read, render,
estimate — not an in-process microbenchmark.

```console
# current synthetic corpus: 100,000 files, 59 bytes/file; cache is warmed before timing
pwsh benches/bench.ps1 -Files 100000 -Runs 5

# or a real checkout
./benches/bench.sh /path/to/linux
```

Each script warms the file cache, then reports min/median/max wall-clock time
and files-per-second for the median run, writing the payload with `--output` so
that stdout is not what is being measured. Set `CTX_TIMING=1` to also print the
walk, filter/read, sort, render, estimate, and write phase times to stderr for
each run:

```console
mkdir -p ~/ctx-bench
TMPDIR=~/ctx-bench CTX_TIMING=1 ./benches/bench.sh
```

### Recorded results

The earlier Windows values and the new WSL2 values below use different
synthetic corpora and are not directly comparable. The earlier Windows run
used approximately 170-byte files; its exact per-file size and cache state
were not recorded. The current benchmark generators both write identical
59-byte UTF-8/LF source bodies and use the same relative paths. For current
script runs, timings are taken after a warm-up run (warm cache); WSL2 results
below also include the environment and cache state for each measurement.

| Environment | Cache state | Corpus | min | median | max | Throughput (median) |
| --- | --- | --- | --- | --- | --- | --- |
| Windows 11 (earlier recorded run) | Not recorded | 100,000 files, approximately 170 bytes/file | 2.29 s | **2.49 s** | 2.52 s | ~40,000 files/s |
| WSL2 Ubuntu on ext4 (this run) | Warm (after one warm-up run) | 100,000 `.rs` files, 59 bytes/file | 0.400 s | **0.471 s** | 0.496 s | 212,193 files/s |

For the WSL2 Ubuntu-on-ext4, warm-cache corpus (100,000 `.rs` files, 59
bytes/file), source files totaled 5,900,000 bytes and the rendered payload was
14,477,816 bytes. The table below reports median phase times over five timed
runs for that same corpus and environment.

| Environment and corpus | Cache state | walk | filter/read | sort | render | estimate | write |
| --- | --- | --- | --- | --- | --- | --- | --- |
| WSL2 Ubuntu on ext4; 100,000 `.rs` files, 59 bytes/file | Warm (after one warm-up run) | 132.094 ms | 114.650 ms | 20.074 ms | 134.687 ms | 19.507 ms | 5.419 ms |

The earlier Windows 11 benchmark-run release executable was recorded at
1,866,752 bytes (1.78 MiB); the CI workflow reports each platform's release
binary size.

Reproduce exactly:

```console
pwsh benches/bench.ps1 -Files 100000 -Runs 5
```

On Linux or macOS:

```console
./benches/bench.sh /path/to/a/large/checkout
```

## Development

```console
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

CI runs formatting, Clippy, tests, and a release build with binary-size output
on Linux, macOS, and Windows. A separate Linux slim-build job builds and tests
with `--no-default-features`; another Linux job runs tests under Xvfb and
exercises the X11 clipboard harness. `benches/` is not a cargo bench target, so
it is never compiled by `cargo test`.

## License

MIT. See [LICENSE](LICENSE).
