#!/usr/bin/env bash
# One binary's symbol cell, run several times under callgrind the way perf.sh runs it (RAYON_NUM_THREADS=1, a warmed
# token cache, a deadline the run cannot reach). The same base binary counted 51.6G to 63.5G across the jobs of one
# probe; named profiles of one binary say which functions move, and every answer is compared byte for byte with the
# warm-up's. Runs go in batches of <parallel>: perf.sh runs its eight cells at once.
# Usage: determinism.sh <bin> <fixture> <out-dir> [runs] [parallel]
set -euo pipefail
BIN=$1; FIX=$2; OUT=$3; RUNS=${4:-4}; PAR=${5:-4}
mkdir -p "$OUT"
args=(--symbol build_diff_context --format json --quiet)
DIFFCTX_TOKEN_CACHE_DIR="$OUT/cache" RAYON_NUM_THREADS=1 "$BIN" "$FIX" "${args[@]}" > "$OUT/warm.json"
one() {
  DIFFCTX_TOKEN_CACHE_DIR="$OUT/cache" RAYON_NUM_THREADS=1 valgrind --tool=callgrind \
    --callgrind-out-file="$OUT/symbol-$1.cg" "$BIN" "$FIX" "${args[@]}" --timeout 86400 \
    > "$OUT/symbol-$1.json" 2> "$OUT/symbol-$1.log"
}
i=0
while [ "$i" -lt "$RUNS" ]; do
  pids=()
  for _ in $(seq "$PAR"); do
    [ "$i" -lt "$RUNS" ] || break
    i=$((i + 1))
    one "$i" &
    pids+=($!)
  done
  for p in "${pids[@]}"; do wait "$p"; done
done
echo "| run ($(basename "$OUT")) | Ir | answer against the warm-up |"
echo "|---|---:|---|"
for k in $(seq "$RUNS"); do
  ir=$(sed -n 's/^summary: //p' "$OUT/symbol-$k.cg")
  if cmp -s "$OUT/warm.json" "$OUT/symbol-$k.json"; then same=same; else same=DIFFERS; fi
  echo "| $k | $ir | $same |"
done
