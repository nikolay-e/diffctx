#!/usr/bin/env bash
# Instructions per cell, base binary against the step's, under callgrind: one run each is exact, where wall time on a
# shared runner is not. diffctx only (git children are not traced), RAYON_NUM_THREADS=1, token cache warmed first.
# Callgrind slows a run some fiftyfold, past the default 300 s deadline: a deadline partial (or the watchdog's 124)
# would count the instructions of a different, shorter run, so each run gets a deadline it cannot reach.
# Usage: perf.sh <base-bin> <step-bin> <fixture> <out-dir>   Exit 1 when a cell grows more than 3%.
set -euo pipefail
B=$1; A=$2; FIX=$3; OUT=$4
mkdir -p "$OUT"
cells=(
  "impact-narrow|--diff 384c4b86..3562b922 --mode impact"
  "pack-ego-wide|--diff 0d75e73a..99ff4abc --scoring ego --mode pack --budget 8000"
  "locate-ppr-narrow|--diff 384c4b86..3562b922 --scoring ppr --mode locate --budget 8000"
  "symbol|--symbol build_diff_context"
)
one() {
  local name=$1 bin=$2 tag=$3 args=$4
  # shellcheck disable=SC2086
  DIFFCTX_TOKEN_CACHE_DIR="$OUT/cache-$tag" RAYON_NUM_THREADS=1 "$bin" "$FIX" $args --format json --quiet >/dev/null
  # shellcheck disable=SC2086
  DIFFCTX_TOKEN_CACHE_DIR="$OUT/cache-$tag" RAYON_NUM_THREADS=1 valgrind --tool=callgrind \
    --callgrind-out-file="$OUT/$name-$tag.cg" "$bin" "$FIX" $args --timeout 86400 --format json --quiet \
    >/dev/null 2>"$OUT/$name-$tag.log"
}
pids=()
for cell in "${cells[@]}"; do
  name=${cell%%|*}; args=${cell#*|}
  one "$name" "$B" base "$args" & pids+=($!)
  one "$name" "$A" step "$args" & pids+=($!)
done
failed=0
for p in "${pids[@]}"; do wait "$p" || failed=1; done
if [ "$failed" = 1 ]; then
  for l in "$OUT"/*.log; do echo "--- $l"; tail -5 "$l"; done
  echo "a cell failed: no instruction count" >&2
  exit 1
fi
python3 - "$OUT" <<'PY'
import pathlib, re, sys
out = pathlib.Path(sys.argv[1])
def total(f):
    m = re.search(r"^(?:totals|summary): (\d+)", f.read_text(), re.M)
    return int(m.group(1))
worst, lines = -1.0, ["| cell | base Ir | step Ir | change |", "|---|---:|---:|---:|"]
for b in sorted(out.glob("*-base.cg")):
    name = b.name[: -len("-base.cg")]
    ib, ia = total(b), total(out / f"{name}-step.cg")
    worst = max(worst, ia / ib - 1)
    lines.append(f"| {name} | {ib:,} | {ia:,} | {ia / ib - 1:+.3%} |")
lines.append(f"\nworst instructions change {worst:+.3%} ({'within' if worst <= 0.03 else 'OVER'} the 3% bound)")
print("\n".join(lines))
sys.exit(0 if worst <= 0.03 else 1)
PY
