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
# shards of everything else, and `rest` is `rest:1/1`. No argument runs it all.
# The shards balance expected seconds, not binary counts: `weights` below,
# heaviest first onto the lightest shard (ties by name), so every caller gets
# the same split for the same N. A suite missing from the table weighs
# `default_weight` and still runs; re-measure the table when a suite grows.
# Every run of the suites appends where its time went to DIFFCTX_PROFILE (a
# temporary file unless set; empty turns it off), summarized when they end.
set -euo pipefail
# The pre-commit hook runs this inside git's environment, and from a linked worktree a suite's
# `git init` that inherited it rewrites the shared .git/config (fj#199). scripts/git-env-gate.sh
# runs the suites without this line to show a spawn that scrubs nothing itself.
# shellcheck source=scripts/repo-locating-env.sh
source "$(dirname "$0")/repo-locating-env.sh"
unset "${REPO_LOCATING_ENV[@]}"
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

# Seconds per suite, measured 2026-11-11 (`lib` is the library's inline tests,
# an estimate).
default_weight=3
weights="
git_faults 117
metamorphic 112
loss_sweep 91
hook_property 89
impact 71
unknowns 68
hook 25
lane_f 24
budget_contract 20
native_cli 17
bindings 11
lib 10
"

# The suites of shard $1 of $2, one name per line (`lib` is the library's tests).
shard_suites() {
  {
    echo "$weights"
    echo lib
    for file in tests/*.rs; do basename "$file" .rs; done | grep -vx yaml_cases
  } | awk -v fallback="$default_weight" '
    NF == 2 { seconds[$1] = $2; next }
    NF == 1 { print (($1 in seconds) ? seconds[$1] : fallback), $1 }' |
    LC_ALL=C sort -k1,1nr -k2,2 |
    awk -v shard="$1" -v shards="$2" '
      BEGIN { for (i = 1; i <= shards; i++) load[i] = 0 }
      {
        lightest = 1
        for (i = 2; i <= shards; i++) if (load[i] < load[lightest]) lightest = i
        load[lightest] += $1
        if (lightest == shard) print $2
      }'
}

status=0
run() { "$@" || status=1; }

if ((run_rest)); then
  # --features python, or analytics/, graph_export/ and project_graph/ are not
  # compiled at all: they are cfg-gated on it and ship inside the wheel.
  names=$(shard_suites "$shard" "$shards")
  suites=()
  for name in $names; do
    if [[ "$name" = lib ]]; then
      run cargo test --locked --lib --features python
    else
      suites+=(--test "$name")
    fi
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
