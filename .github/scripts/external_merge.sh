#!/usr/bin/env bash
# Merge a dataset's shard outputs into one summary.
#
#   external_merge.sh DATASET SHARDS_DIR OUT_DIR ENGINE_REF
#
# SHARDS_DIR holds one `shard-*` directory per shard, as `external_run.sh`
# wrote them. EXTERNAL_WORK is scratch space (default: a fresh temp dir).
set -euo pipefail

dataset="${1:?dataset}"
shards_dir="${2:?shards dir}"
out="${3:?out dir}"
engine_ref="${4:-}"
work="${EXTERNAL_WORK:-$(mktemp -d)}"
mkdir -p "${out}" "${work}"
shopt -s nullglob

merge_arb_edit2ripple() {
  pins=datasets/external-pins/arb_edit2ripple.json
  repository="$(python -c "import json,sys; print(json.load(open(sys.argv[1]))['evaluator']['repository'])" "${pins}")"
  commit="$(python -c "import json,sys; print(json.load(open(sys.argv[1]))['evaluator']['commit'])" "${pins}")"

  # The benchmark's own evaluator at the pinned commit, in an environment that
  # holds nothing of ours.
  git clone --quiet "${repository}" "${work}/arb-src"
  git -C "${work}/arb-src" checkout --quiet "${commit}"
  [ "$(git -C "${work}/arb-src" rev-parse HEAD)" = "${commit}" ]
  python -m venv "${work}/arb-venv"
  "${work}/arb-venv/bin/pip" install --quiet "${work}/arb-src"

  python -m eval arb-edit2ripple fetch --out "${work}/arb"
  rankings=("${shards_dir}"/shard-*/rankings.jsonl)
  # A full run must rank every sample; EXTERNAL_LIMIT marks a local smoke.
  require_all=(--require-all)
  if [ -n "${EXTERNAL_LIMIT:-}" ]; then require_all=(); fi
  "${work}/arb-venv/bin/python" -I .github/scripts/arb_score.py \
    --data-root "${work}/arb/data" \
    --rankings "${rankings[@]}" \
    --out "${out}" \
    --engine-ref "${engine_ref}" \
    --evaluator-commit "${commit}" \
    ${require_all[@]+"${require_all[@]}"}

  # The benchmark's own baselines under both seedings, over the samples diffctx ranked. They do not
  # depend on the engine, so a run recomputes them only when EXTERNAL_BASELINES=1.
  if [ "${EXTERNAL_BASELINES:-0}" = 1 ]; then
    python -m eval arb-edit2ripple baseline-inputs --data-root "${work}/arb/data" --rankings "${rankings[@]}" --out "${work}/inputs"
    limit=()
    if [ -n "${EXTERNAL_LIMIT:-}" ]; then limit=(--limit "${EXTERNAL_LIMIT}"); fi
    "${work}/arb-venv/bin/python" -I .github/scripts/arb_baselines.py \
      --data-root "${work}/arb/data" \
      --inputs "${work}/inputs" \
      ${limit[@]+"${limit[@]}"} \
      --out "${out}/baselines.json"
  fi
}

merge_contextbench_nontrivial() {
  dirs=("${shards_dir}"/shard-*)
  python -m eval cb-nontrivial summarize --shard-dirs "${dirs[@]}" --out "${out}/summary.json" --engine-ref "${engine_ref}"
}

# The row-file runners print their summary on stdout.
merge_rows() {
  rows=("${shards_dir}"/shard-*/rows.json)
  python -m eval "$1" summarize --rows "${rows[@]}" >"${out}/summary.json"
  cp -r "${shards_dir}"/shard-* "${out}/"
}

case "${dataset}" in
swerebench_infra) merge_rows swerebench-infra ;;
cfgnet500) merge_rows cfgnet500 ;;
arb_edit2ripple) merge_arb_edit2ripple ;;
contextbench_nontrivial) merge_contextbench_nontrivial ;;
*)
  echo "unknown external dataset: ${dataset}" >&2
  exit 2
  ;;
esac
