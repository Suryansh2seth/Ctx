# Project instructions

- Keep `#![forbid(unsafe_code)]`; do not add unsafe Rust.
- Do not spawn subprocesses from `src\`. Clipboard access must use the existing in-process API.
- stdout is reserved for the generated payload. Send diagnostics and progress to stderr.
- Report a command, test, build, or benchmark as run only when it was actually executed; distinguish configured CI checks from observed results.
