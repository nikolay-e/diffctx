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
# An argument runs a part, for a runner that cannot hold them all inside its
# time limit: `corpus` is the YAML corpus alone, `rest:K/N` the K-th of N
# shards of everything else (the library's tests ride in the first), and
# `rest` is `rest:1/1`. No argument runs it all.
# Every run of the suites appends where its time went to DIFFCTX_PROFILE (a
# temporary file unless set; empty turns it off), summarized when they end.
set -euo pipefail
cd "$(dirname "$0")/../crates/diffctx-native"

part=${1:-all}
run_corpus=1
run_rest=1
shard=1
shards=1
case "$part" in
all) ;;
corpus) run_rest=0 ;;
rest) run_corpus=0 ;;
rest:[1-9]*/[1-9]*)
  run_corpus=0
  shard=${part#rest:}
  shards=${shard#*/}
  shard=${shard%/*}
  ((shard <= shards)) || {
    echo "shard $shard of $shards" >&2
    exit 2
  }
  ;;
*)
  echo "usage: $0 [all|corpus|rest|rest:K/N]" >&2
  exit 2
  ;;
esac

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

if ((run_rest)); then
  # --features python, or analytics/, graph_export/ and project_graph/ are not
  # compiled at all: they are cfg-gated on it and ship inside the wheel.
  if ((shard == 1)); then
    run cargo test --locked --lib --features python
  fi

  suites=()
  position=0
  for file in tests/*.rs; do
    name=$(basename "$file" .rs)
    [[ "$name" = yaml_cases ]] && continue
    if ((position % shards + 1 == shard)); then suites+=(--test "$name"); fi
    position=$((position + 1))
  done
  # One failing suite must not hide the others.
  if ((${#suites[@]})); then
    run cargo test --locked --no-fail-fast "${suites[@]}"
  fi
fi

if ((run_corpus)); then
  run cargo test --release --locked --test yaml_cases
fi
exit "$status"
