#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CTX="${CTX:-$ROOT/target/release/ctx}"
TMP="$(mktemp -d)"
manager_pid=
cleanup() {
  if [[ -n "$manager_pid" ]]; then
    kill "$manager_pid" 2>/dev/null || true
    wait "$manager_pid" 2>/dev/null || true
  fi
  rm -rf "$TMP"
}
trap cleanup EXIT

if [[ ! -x "$CTX" ]]; then
  echo "ctx release binary not found at $CTX" >&2
  exit 1
fi
command -v timeout >/dev/null
command -v xclip >/dev/null

mkdir -p "$TMP/tree"
printf 'fn sample() -> u8 { 7 }\n' > "$TMP/tree/sample.rs"
(
  cd "$TMP/tree"
  "$CTX" --format plain . > "$TMP/expected" 2> "$TMP/expected.stderr"
)

start_ns="$(date +%s%N)"
(
  cd "$TMP/tree"
  timeout 10s "$CTX" --copy --format plain . > "$TMP/copy.stdout" 2> "$TMP/copy.stderr"
) &
ctx_pid=$!
copied=0
deadline=$((SECONDS + 9))
while kill -0 "$ctx_pid" 2>/dev/null && (( SECONDS < deadline )); do
  if timeout 1s xclip -selection clipboard -o > "$TMP/clipboard" 2>/dev/null; then
    copied=1
    break
  fi
  sleep 0.1
done
if (( copied != 1 )); then
  echo "ctx --copy: clipboard content was not observed before exit/timeout" >&2
  cat "$TMP/copy.stderr" >&2
  exit 1
fi
xclip -selection clipboard -i -quiet < "$TMP/expected" >/dev/null 2>&1 &
manager_pid=$!
read_ns="$(date +%s%N)"
if wait "$ctx_pid"; then
  copy_status=0
else
  copy_status=$?
fi
exit_ns="$(date +%s%N)"
if (( copy_status != 0 )); then
  echo "ctx --copy exited with status $copy_status after clipboard ownership transfer" >&2
  cat "$TMP/copy.stderr" >&2
  exit 1
fi
if ! timeout 1s xclip -selection clipboard -o > "$TMP/clipboard-after-exit" 2>/dev/null; then
  echo "ctx --copy exited, but the clipboard was unavailable afterward" >&2
  cat "$TMP/copy.stderr" >&2
  exit 1
fi
if ! cmp -s "$TMP/expected" "$TMP/clipboard"; then
  echo "ctx --copy clipboard content differed from the generated payload" >&2
  exit 1
fi
if ! cmp -s "$TMP/expected" "$TMP/clipboard-after-exit"; then
  echo "ctx --copy clipboard content changed after exit" >&2
  exit 1
fi
if [[ -s "$TMP/copy.stdout" ]]; then
  echo "ctx --copy unexpectedly wrote payload to stdout" >&2
  exit 1
fi
if ! grep -Fq 'waiting for X11 clipboard ownership transfer' "$TMP/copy.stderr"; then
  echo "ctx --copy did not print the pre-handoff stderr notice" >&2
  cat "$TMP/copy.stderr" >&2
  exit 1
fi

awk -v start="$start_ns" -v read="$read_ns" -v finish="$exit_ns" \
  'BEGIN { printf "ctx --copy: clipboard readable after %.3fs; ownership replaced and exited 0 after %.3fs; content verified after exit\n", (read-start)/1000000000, (finish-start)/1000000000 }'

set +e
(
  cd "$TMP/tree"
  env -u DISPLAY -u WAYLAND_DISPLAY timeout 10s "$CTX" --copy \
    --output "$TMP/no-display.payload" --format plain . \
    > "$TMP/no-display.stdout" 2> "$TMP/no-display.stderr"
)
no_display_status=$?
set -e

if (( no_display_status != 1 )); then
  echo "no-display ctx --copy exited $no_display_status; expected 1" >&2
  cat "$TMP/no-display.stderr" >&2
  exit 1
fi
if [[ -s "$TMP/no-display.stdout" ]]; then
  echo "no-display ctx --copy unexpectedly wrote to stdout" >&2
  exit 1
fi
if ! grep -Fq 'hint:' "$TMP/no-display.stderr"; then
  echo "no-display ctx --copy did not print a stderr hint" >&2
  cat "$TMP/no-display.stderr" >&2
  exit 1
fi
if ! cmp -s "$TMP/expected" "$TMP/no-display.payload"; then
  echo "no-display ctx --copy did not preserve the payload in --output" >&2
  exit 1
fi
echo "no-display ctx --copy: exit 1, empty stdout, stderr hint, --output payload preserved"
