#!/usr/bin/env bash
# Times `ctx` on a large tree (Linux / macOS).
#
# Without a path argument this generates a synthetic tree of $FILES small files
# in nested directories, then times `ctx` on it with a warm file cache.
# With a path argument it times ctx on a real checkout instead.
#
# This is a script rather than a criterion benchmark on purpose: the thing worth
# measuring is a whole-process run over a real filesystem (walk + read + render),
# not an in-process microbenchmark.
#
# Usage:
#   ./benches/bench.sh                      # 100k synthetic files
#   FILES=50000 RUNS=3 ./benches/bench.sh
#   ./benches/bench.sh /path/to/linux        # a real checkout
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CTX="${CTX:-$ROOT/target/release/ctx}"
FILES="${FILES:-100000}"
RUNS="${RUNS:-5}"
TARGET="${1:-}"

if [[ ! -x "$CTX" ]]; then
  echo "ctx binary not found at $CTX. Run: cargo build --release" >&2
  exit 1
fi
CTX="$(cd "$(dirname "$CTX")" && pwd)/$(basename "$CTX")"
TMPDIR="${TMPDIR:-/tmp}"
TMPDIR="$(cd "$TMPDIR" && pwd)"
export TMPDIR

cleanup() { [[ -n "${GENERATED:-}" ]] && rm -rf "$GENERATED"; }
trap cleanup EXIT

if [[ -z "$TARGET" ]]; then
  TARGET="$(mktemp -d)"
  GENERATED="$TARGET"
  echo "generating $FILES files under $TARGET ..."
  # With these coordinates, the directory pair repeats every 500 files.
  dir_count=$FILES
  if ((dir_count > 500)); then dir_count=500; fi
  for ((i = 0; i < dir_count; i++)); do
    d="$TARGET/src/mod$(printf '%04d' $((i % 500)))/sub$(printf '%03d' $((i % 100)))"
    mkdir -p "$d"
  done
  # Parallelize file creation; timed ctx runs remain unchanged.
  jobs=8
  files_per_job=$(((FILES + jobs - 1) / jobs))
  pids=()
  for ((worker = 0; worker < jobs; worker++)); do
    start=$((worker * files_per_job))
    if ((start >= FILES)); then break; fi
    end=$((start + files_per_job))
    if ((end > FILES)); then end=$FILES; fi
    (
      for ((i = start; i < end; i++)); do
        d="$TARGET/src/mod$(printf '%04d' $((i % 500)))/sub$(printf '%03d' $((i % 100)))"
        printf 'pub fn item() -> u32 {\n    let value = 41;\n    value + 1\n}\n' > "$d/file$i.rs"
      done
    ) &
    pids+=("$!")
  done
  for pid in "${pids[@]}"; do
    wait "$pid"
  done
fi

TARGET="$(cd "$TARGET" && pwd)"
count=$(find "$TARGET" -type f | wc -l | tr -d ' ')
echo "tree: $TARGET ($count files)"
echo "ctx:  $CTX"
echo

TIMES_FILE="${TMPDIR:-/tmp}/ctx-bench-times.txt"
OUTPUT_FILE="${TMPDIR:-/tmp}/ctx-bench-out.txt"
(
  cd "$TARGET"
  echo "--- warm-up ---"
  "$CTX" --quiet . > /dev/null

  echo "--- $RUNS timed runs ---"
  times=()
  for ((r = 1; r <= RUNS; r++)); do
    start=$(date +%s.%N)
    "$CTX" --quiet --output "$OUTPUT_FILE" .
    end=$(date +%s.%N)
    elapsed=$(awk -v start="$start" -v end="$end" 'BEGIN { printf "%.9f", end - start }')
    times+=("$elapsed")
    printf 'run %d: %.3fs\n' "$r" "$elapsed"
  done

  printf '%s\n' "${times[@]}" | sort -n > "$TIMES_FILE"
  min=$(head -n1 "$TIMES_FILE")
  max=$(tail -n1 "$TIMES_FILE")
  median=$(awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}' "$TIMES_FILE")

  echo
  echo "files:      $count"
  echo "runs:       $RUNS"
  printf 'min:        %.3fs\n' "$min"
  printf 'median:     %.3fs\n' "$median"
  printf 'max:        %.3fs\n' "$max"
  if [[ "$count" -gt 0 ]]; then
    awk -v c="$count" -v m="$median" 'BEGIN{printf "throughput: %.0f files/s (median)\n", c/m}'
  fi
)
