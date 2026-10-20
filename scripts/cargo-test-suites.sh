#!/usr/bin/env bash
# Every Rust suite the tree holds, as CI and the pre-commit hook run them: the
# library's inline tests, each integration suite under tests/, then the YAML
# corpus on the release profile the wheel ships with. The suites are read off
# the directory: a list named in each caller left the 1.20.0 gates (unknowns,
# lane_*) running in no job, after the same list had already hidden the
# binary-level suites until 2026-10-11.
# DIFFCTX_YAML_CASES_LIMIT, when set, samples the corpus.
# Every run of the suites appends where its time went to DIFFCTX_PROFILE (a
# temporary file unless set; empty turns it off), summarized when they end.
set -euo pipefail
cd "$(dirname "$0")/../crates/diffctx-native"

if [ -z "${DIFFCTX_PROFILE+set}" ]; then
  DIFFCTX_PROFILE=$(mktemp "${TMPDIR:-/tmp}/diffctx-profile-XXXXXX")
  owned_profile=$DIFFCTX_PROFILE
  export DIFFCTX_PROFILE
fi
report() {
  local python
  python=$(command -v python3 || command -v python || true)
  if [ -n "${DIFFCTX_PROFILE:-}" ] && [ -n "$python" ]; then
    "$python" ../../tests/framework/profile_report.py "$DIFFCTX_PROFILE" || true
  fi
  [ -z "${owned_profile:-}" ] || rm -f "$owned_profile"
}
trap report EXIT

# --features python, or analytics/, graph_export/ and project_graph/ are not
# compiled at all: they are cfg-gated on it and ship inside the wheel.
cargo test --lib --features python

suites=()
for file in tests/*.rs; do
  name=$(basename "$file" .rs)
  [ "$name" = yaml_cases ] || suites+=(--test "$name")
done
# One failing suite must not hide the others.
cargo test --no-fail-fast "${suites[@]}"

cargo test --release --test yaml_cases
