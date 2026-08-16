#!/bin/bash
# Stage A screening grid: admission{0,1} x tau{0.05,0.08,0.12} x mode{ego,pit}.
# Corpus probe (kill gate) + dcbench 372 (ranking) per cell, sequential.
set -u
cd "$(dirname "$0")/../.." || exit 1
OUT=results/stageA
mkdir -p "$OUT"

for adm in 0 1; do
  for tau in 0.05 0.08 0.12; do
    for mode in ego pit; do
      cell="adm${adm}_tau${tau}_${mode}"
      if [[ -f "$OUT/$cell.done" ]]; then
        echo "=== skip $cell (done)"
        continue
      fi
      echo "=== $(date +%F' '%T) $cell corpus"
      envs=(DIFFCTX_YAML_IGNORE_BASELINE=1 "DIFFCTX_PROBE_TAU=$tau")
      [[ "$adm" == 1 ]] && envs+=(DIFFCTX_FILE_ADMISSION=1)
      [[ "$mode" == pit ]] && envs+=(DIFFCTX_PROBE_MODE=pit DIFFCTX_PIT_BLEND=1.0)
      env "${envs[@]}" cargo test --profile release-unwind --test yaml_cases 2>&1 |
        tail -400 >"$OUT/$cell.corpus.txt"
      grep "test result" "$OUT/$cell.corpus.txt" || echo "corpus: no result line"

      echo "=== $(date +%F' '%T) $cell dcbench"
      dcargs=(--mode "$mode" --tau "$tau" --out "$OUT/$cell" --budget 8000 --timeout 60)
      [[ "$adm" == 1 ]] && dcargs+=(--env DIFFCTX_FILE_ADMISSION=1)
      [[ "$mode" == pit ]] && dcargs+=(--env DIFFCTX_PIT_BLEND=1.0)
      if python -m eval dcbench-score "${dcargs[@]}" >"$OUT/$cell.dcbench.log" 2>&1; then
        touch "$OUT/$cell.done"
        echo "=== $(date +%F' '%T) $cell done"
      else
        echo "=== $(date +%F' '%T) $cell FAILED (see $OUT/$cell.dcbench.log) — left undone for resume"
      fi
    done
  done
done
echo "STAGE A COMPLETE $(date +%F' '%T)"
