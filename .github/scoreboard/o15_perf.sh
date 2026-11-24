#!/usr/bin/env bash
# Instructions of one binary on the pack and locate cells with the edges drawn by the old builders
# (DIFFCTX_CELL_EDGES=none) and from the facts' cells (=all), under callgrind: one run each is exact where wall time
# on a shared runner is not. RAYON_NUM_THREADS=1, token cache warmed first, a deadline callgrind cannot reach, at most one
# run per CPU. `none` runs twice: a cell whose two runs differ by more than 1% makes the run invalid (exit 2).
# The answers of the callgrind runs are kept: whether `all` changes the selection is read from them.
# Usage: o15_perf.sh <bin> <fixture> <out-dir>
set -euo pipefail
BIN=$1
FIX=$2
OUT=$3
mkdir -p "$OUT"
cells=(
  "pack-ego-wide|--diff 0d75e73a..99ff4abc --scoring ego --mode pack --budget 8000"
  "pack-ppr-narrow|--diff 384c4b86..3562b922 --scoring ppr --mode pack --budget 8000"
  "locate-ego-wide|--diff 0d75e73a..99ff4abc --scoring ego --mode locate --budget 8000"
  "locate-ppr-narrow|--diff 384c4b86..3562b922 --scoring ppr --mode locate --budget 8000"
)
one() {
  local name=$1 edges=$2 tag=$3 args=$4
  # shellcheck disable=SC2086
  DIFFCTX_CELL_EDGES=$edges DIFFCTX_TOKEN_CACHE_DIR="$OUT/cache-$tag" RAYON_NUM_THREADS=1 \
    "$BIN" "$FIX" $args --format json --quiet >/dev/null
  # shellcheck disable=SC2086
  DIFFCTX_CELL_EDGES=$edges DIFFCTX_TOKEN_CACHE_DIR="$OUT/cache-$tag" RAYON_NUM_THREADS=1 valgrind --tool=callgrind \
    --callgrind-out-file="$OUT/$name-$tag.cg" "$BIN" "$FIX" $args --timeout 86400 --format json --quiet \
    >"$OUT/$name-$tag.json" 2>"$OUT/$name-$tag.log"
}
slots=$(nproc)
failed=0
running=0
for cell in "${cells[@]}"; do
  name=${cell%%|*}
  args=${cell#*|}
  for run in "none none" "none-control none" "all all"; do
    tag=${run%% *}
    edges=${run#* }
    if [ "$running" -ge "$slots" ]; then
      wait -n || failed=1
      running=$((running - 1))
    fi
    one "$name" "$edges" "$tag" "$args" &
    running=$((running + 1))
  done
done
while [ "$running" -gt 0 ]; do
  wait -n || failed=1
  running=$((running - 1))
done
if [ "$failed" = 1 ]; then
  for l in "$OUT"/*.log; do
    echo "--- $l"
    tail -5 "$l"
  done
  echo "a cell failed: no instruction count" >&2
  exit 1
fi
python3 -I - "$OUT" <<'PY'
import json, pathlib, re, sys
out = pathlib.Path(sys.argv[1])
def total(f):
    return int(re.search(r"^(?:totals|summary): (\d+)", f.read_text(), re.M).group(1))
def answer(f):
    doc = json.loads(f.read_text())
    for key in ("latency", "provenance"):
        doc.pop(key, None)
    return doc
noise = 0.0
lines = ["| cell | none Ir | all Ir | ratio all/none | none run twice | answer with all |", "|---|---:|---:|---:|---:|---|"]
for n in sorted(out.glob("*-none.cg")):
    name = n.name[: -len("-none.cg")]
    i0, i1, ic = total(n), total(out / f"{name}-all.cg"), total(out / f"{name}-none-control.cg")
    noise = max(noise, abs(ic / i0 - 1))
    same = "identical" if answer(out / f"{name}-none.json") == answer(out / f"{name}-all.json") else "DIFFERENT"
    lines.append(f"| {name} | {i0:,} | {i1:,} | {i1 / i0:.3f}x ({i1 / i0 - 1:+.2%}) | {ic / i0 - 1:+.3%} | {same} |")
lines.append(f"\nnone against itself: worst {noise:.3%} ({'valid' if noise <= 0.01 else 'INVALID: over 1%, no number here counts'})")
print("\n".join(lines))
sys.exit(2 if noise > 0.01 else 0)
PY
