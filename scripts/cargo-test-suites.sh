#!/usr/bin/env bash
# Every Rust suite the tree holds, as CI and the pre-commit hook run them: the
# library's inline tests, each integration suite under tests/, then the YAML
# corpus on the release profile the wheel ships with. The suites are read off
# the directory: a list named in each caller left the 1.20.0 gates (unknowns,
# lane_*) running in no job, after the same list had already hidden the
# binary-level suites until 2026-10-11.
# DIFFCTX_YAML_CASES_LIMIT, when set, samples the corpus.
set -euo pipefail
cd "$(dirname "$0")/../crates/diffctx-native"

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
