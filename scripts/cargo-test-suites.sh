#!/usr/bin/env bash
# Every Rust suite the tree holds, as CI and the pre-commit hook run them: the
# library's inline tests, each integration suite under tests/, then the YAML
# corpus on the release profile the wheel ships with. The suites are read off
# the directory: a list named in each caller left the 1.20.0 gates (unknowns,
# lane_*) running in no job, after the same list had already hidden the
# binary-level suites until 2026-10-11.
# A failing run does not stop the ones after it; the script still exits
# non-zero.
# DIFFCTX_YAML_CASES_LIMIT, when set, samples the corpus.
# Every run of the suites appends where its time went to DIFFCTX_PROFILE (a
# temporary file unless set; empty turns it off), summarized when they end.
set -euo pipefail
cd "$(dirname "$0")/../crates/diffctx-native"

if [[ -z "${DIFFCTX_PROFILE+set}" ]]; then
  DIFFCTX_PROFILE=$(mktemp "${TMPDIR:-/tmp}/diffctx-profile-XXXXXX")
  owned_profile=$DIFFCTX_PROFILE
  export DIFFCTX_PROFILE
fi
# shellcheck disable=SC2329  # run by the EXIT trap
report() {
  local python
  python=$(command -v python3 || command -v python || true)
  if [[ -n "${DIFFCTX_PROFILE:-}" ]] && [[ -n "$python" ]]; then
    "$python" ../../tests/framework/profile_report.py "$DIFFCTX_PROFILE" || true
  fi
  [[ -z "${owned_profile:-}" ]] || rm -f "$owned_profile"
}
trap report EXIT

status=0
run() { "$@" || status=1; }

# --features python, or analytics/, graph_export/ and project_graph/ are not
# compiled at all: they are cfg-gated on it and ship inside the wheel.
run cargo test --locked --lib --features python

suites=()
for file in tests/*.rs; do
  name=$(basename "$file" .rs)
  [[ "$name" = yaml_cases ]] || suites+=(--test "$name")
done
# One failing suite must not hide the others.
run cargo test --locked --no-fail-fast "${suites[@]}"

run cargo test --release --locked --test yaml_cases
exit "$status"
