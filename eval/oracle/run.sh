#!/usr/bin/env bash
# One local oracle round: install the pinned indexers, measure, time, report.
#
#   eval/oracle/run.sh [--repo NAME]... [--commits N] [--fast] [--out DIR] [--no-install] [--retry-failed]
#
# DIFFCTX_BIN is the diffctx under test (default: target/release/diffctx). Optional inputs to the report:
# DIFFCTX_ORACLE_REPORT (the YAML corpus), ORACLE_METAMORPHIC_REPORT and ORACLE_FAULTS_REPORT (a JSON file or
# a `cargo test` log each).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "${here}/../.." && pwd)"
export DIFFCTX_BIN="${DIFFCTX_BIN:-${root}/target/release/diffctx}"
out="${here}/out/latest"
select_args=()
measure_args=()
while [ $# -gt 0 ]; do
  case "$1" in
  --out)
    out="$2"
    shift 2
    ;;
  --repo | --commits)
    select_args+=("$1" "$2")
    shift 2
    ;;
  --fast)
    select_args+=("$1")
    shift
    ;;
  --no-install | --retry-failed)
    measure_args+=("$1")
    shift
    ;;
  *)
    echo "unknown option: $1" >&2
    exit 2
    ;;
  esac
done

if [ ! -x "${DIFFCTX_BIN}" ]; then
  echo "DIFFCTX_BIN=${DIFFCTX_BIN} is not an executable; build it: cargo build --release --bin diffctx" >&2
  exit 2
fi

oracle() { (cd "${here}" && uv run --frozen python run.py "$@"); }

report_args=(--run "${out}" --bench "${out}/bench.json")
[ -n "${DIFFCTX_ORACLE_REPORT:-}" ] && report_args+=(--harness "${DIFFCTX_ORACLE_REPORT}")
[ -n "${ORACLE_METAMORPHIC_REPORT:-}" ] && report_args+=(--metamorphic "${ORACLE_METAMORPHIC_REPORT}")
[ -n "${ORACLE_FAULTS_REPORT:-}" ] && report_args+=(--faults "${ORACLE_FAULTS_REPORT}")

oracle setup
oracle measure ${select_args[@]+"${select_args[@]}"} ${measure_args[@]+"${measure_args[@]}"} --out "${out}"
oracle bench ${select_args[@]+"${select_args[@]}"} --out "${out}/bench.json"
oracle report "${report_args[@]}"
