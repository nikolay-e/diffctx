#!/usr/bin/env bash
# One shard of an external dataset: rank (or run) with the diffctx build under test.
#
#   external_run.sh DATASET SHARD SHARDS OUT_DIR
#
# Expects the repository checkout as the working directory (the harness) and a
# Python environment with the engine wheel and the `eval` dependency group.
# EXTERNAL_WORK is scratch space for downloads and clones (default: a fresh temp
# dir); EXTERNAL_LIMIT caps the samples, for a local smoke only.
set -euo pipefail

dataset="${1:?dataset}"
shard="${2:?shard}"
shards="${3:?shards}"
out="${4:?out dir}"
work="${EXTERNAL_WORK:-$(mktemp -d)}"
mkdir -p "${out}" "${work}"

run_arb_edit2ripple() {
  python -m eval arb-edit2ripple fetch --out "${work}/arb"
  limit=()
  if [ -n "${EXTERNAL_LIMIT:-}" ]; then limit=(--limit "${EXTERNAL_LIMIT}"); fi
  python -m eval arb-edit2ripple run \
    --data-root "${work}/arb/data" \
    --repos-dir "${work}/repos" \
    --shard "${shard}/${shards}" \
    --timeout 600 \
    ${limit[@]+"${limit[@]}"} \
    --out "${out}"
}

# The sweep's own machinery on the nontrivial Verified instances, so the numbers
# are the sweep's numbers. The operating point is read from the engine, never
# written here (#175).
run_contextbench_nontrivial() {
  python -m eval cb-nontrivial prepare --shards "${shards}" --out "${work}/plan"
  python - >"${work}/winner.json" <<'PY'
import json

from diffctx._diffctx import DEFAULT_CORE_BUDGET_FRACTION, DEFAULT_TAU

print(json.dumps({"winner": {"tau": DEFAULT_TAU, "core_budget_fraction": DEFAULT_CORE_BUDGET_FRACTION,
                             "budget": 8000, "scoring": "ego", "extra_env": {}}}))
PY
  limit=()
  if [ -n "${EXTERNAL_LIMIT:-}" ]; then limit=(--limit "${EXTERNAL_LIMIT}"); fi
  python -m eval run-final \
    --baseline diffctx \
    --winner "${work}/winner.json" \
    --manifests-dir "${work}/plan/shard-${shard}" \
    --repos-dir "${work}/repos" \
    --workers 3 \
    --timeout-per-instance 600 \
    --min-memory-gb 2 \
    --min-disk-gb 5 \
    ${limit[@]+"${limit[@]}"} \
    --out "${out}"
}

# Whole repositories per shard, so a shard clones only the repositories it owns.
run_swerebench_infra() {
  ids="$(
    python - "${shard}" "${shards}" <<'PY'
import json
import sys

from eval.external.sharding import shard_items

rows = json.load(open("datasets/xlang/swerebench_infra.json"))["instances"]
index, total = int(sys.argv[1]), int(sys.argv[2])
print(",".join(r["instance_id"] for r in shard_items(rows, lambda r: r["repo"], total, index)))
PY
  )"
  limit=()
  if [ -n "${EXTERNAL_LIMIT:-}" ]; then limit=(--limit "${EXTERNAL_LIMIT}"); fi
  SWE_REBENCH_REPOS_DIR="${work}/repos" python -m eval swerebench-infra run \
    --ids "${ids}" \
    --hf-cache "${work}/hf" \
    --workers 3 \
    ${limit[@]+"${limit[@]}"} \
    --out "${out}"
}

run_cfgnet500() {
  python -m eval cfgnet500 run --workdir "${work}/cfgnet" --out "${out}"
}

case "${dataset}" in
swerebench_infra) run_swerebench_infra ;;
cfgnet500) run_cfgnet500 ;;
arb_edit2ripple) run_arb_edit2ripple ;;
contextbench_nontrivial) run_contextbench_nontrivial ;;
*)
  echo "unknown external dataset: ${dataset}" >&2
  exit 2
  ;;
esac
