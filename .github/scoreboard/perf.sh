#!/usr/bin/env bash
# Instructions per cell, base binary against the step's, under callgrind: one run each is exact, where wall time on a
# shared runner is not. diffctx only (git children are not traced), RAYON_NUM_THREADS=1, token cache warmed first.
# Callgrind slows a run some fiftyfold, past the default 300 s deadline: a deadline partial (or the watchdog's 124)
# would count the instructions of a different, shorter run, so each run gets a deadline it cannot reach.
# The base binary runs twice: a cell whose two base runs differ by more than 1% makes the run invalid (exit 2), since
# the same binary once did a quarter more work on the symbol cell.
# Usage: [LABEL_BASE=1.20.0 LABEL_STEP=head] perf.sh <base-bin> <step-bin> <fixture> <out-dir>
# Exit 1 when a cell grows more than 3%, 2 when the control spreads over 1%.
set -euo pipefail
B=$1
A=$2
FIX=$3
OUT=$4
mkdir -p "$OUT"
# shellcheck source=.github/scoreboard/cells.sh
source "$(dirname "$0")/cells.sh"
one() {
  local name=$1 bin=$2 tag=$3 args=$4 tree=$5
  # shellcheck disable=SC2086
  DIFFCTX_TOKEN_CACHE_DIR="$OUT/cache-$tag" RAYON_NUM_THREADS=1 "$bin" "$tree" $args --format json --quiet >/dev/null
  # shellcheck disable=SC2086
  DIFFCTX_TOKEN_CACHE_DIR="$OUT/cache-$tag" RAYON_NUM_THREADS=1 valgrind --tool=callgrind \
    --callgrind-out-file="$OUT/$name-$tag.cg" "$bin" "$tree" $args --timeout 86400 --format json --quiet \
    >/dev/null 2>"$OUT/$name-$tag.log"
}
# At most one run per CPU: oversubscribed runs wait on their git children for longer, and the waiting is counted
# (12 runs on 4 CPUs moved the base binary against itself by 1.5%; 4 on 4 by 0.004%).
slots=$(nproc)
failed=0
running=0
for cell in "${cells[@]}"; do
  name=${cell%%|*}
  args=${cell#*|}
  tree=$(tree_for "$args")
  for run in "base $B" "control $B" "step $A"; do
    if [ "$running" -ge "$slots" ]; then
      wait -n || failed=1
      running=$((running - 1))
    fi
    one "$name" "${run#* }" "${run%% *}" "$args" "$tree" &
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
LABEL_BASE=${LABEL_BASE:-base} LABEL_STEP=${LABEL_STEP:-step} python3 - "$OUT" <<'PY'
import os, pathlib, re, sys
out = pathlib.Path(sys.argv[1])
def total(f):
    m = re.search(r"^(?:totals|summary): (\d+)", f.read_text(), re.M)
    return int(m.group(1))
lb, ls = os.environ["LABEL_BASE"], os.environ["LABEL_STEP"]
worst, noise, lines = -1.0, 0.0, [f"| cell | {lb} Ir | {ls} Ir | ratio {ls}/{lb} | {lb} run twice |", "|---|---:|---:|---:|---:|"]
for b in sorted(out.glob("*-base.cg")):
    name = b.name[: -len("-base.cg")]
    ib, ia, ic = total(b), total(out / f"{name}-step.cg"), total(out / f"{name}-control.cg")
    worst = max(worst, ia / ib - 1)
    noise = max(noise, abs(ic / ib - 1))
    lines.append(f"| {name} | {ib:,} | {ia:,} | {ia / ib:.3f}x ({ia / ib - 1:+.2%}) | {ic / ib - 1:+.3%} |")
lines.append(f"\nworst instructions change {worst:+.3%} ({'within' if worst <= 0.03 else 'OVER'} the 3% bound)")
lines.append(f"{lb} against itself: worst {noise:.3%} ({'valid' if noise <= 0.01 else 'INVALID: over 1%, no number here counts'})")
print("\n".join(lines))
sys.exit(2 if noise > 0.01 else 0 if worst <= 0.03 else 1)
PY
