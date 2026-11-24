#!/usr/bin/env bash
# Every Rust suite the tree holds, as CI and the pre-commit hook run them: the
# library's inline tests, each integration suite under tests/, then the YAML
# corpus on the release profile the wheel ships with. The suites are read off
# the directory: a list named in each caller left the 1.20.0 gates (unknowns,
# lane_*) running in no job, after the same list had already hidden the
# binary-level suites until 2026-10-11. The two corpus suites (yaml_cases and
# class_completeness_corpus, which runs the class-completeness properties over
# every case) are the exception: they run on the release profile.
# A failing run does not stop the ones after it; the script still exits
# non-zero.
# DIFFCTX_YAML_CASES_LIMIT, when set, samples the corpus.
# An argument runs a part, for a runner that cannot hold them all inside its
# time limit: `corpus` is the two corpus suites alone, `rest` everything else,
# and `corpus:K/N` or `rest:K/N` the K-th of N shards of either. No argument
# runs it all.
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
corpus | corpus:[1-9]*/[1-9]*) run_rest=0 ;;
rest | rest:[1-9]*/[1-9]*) run_corpus=0 ;;
*)
  echo "usage: $0 [all|corpus|rest|corpus:K/N|rest:K/N]" >&2
  exit 2
  ;;
esac
if [[ "$part" = *:* ]]; then
  shard=${part#*:}
  shards=${shard#*/}
  shard=${shard%/*}
  ((shard <= shards)) || {
    echo "shard $shard of $shards" >&2
    exit 2
  }
fi

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

# Seconds per suite, one column per OS (linux, macos, windows), from the runs of
# 2026-11-24 (`lib` is the library's inline tests; the class corpus is also split
# by case, DIFFCTX_CLASS_SHARD). Suites under five seconds on every OS are not
# listed: they weigh `default_weight`. The shards balance the seconds of the OS
# that runs them: a suite is slow on one OS and cheap on another (loss_sweep,
# unknowns on Windows, git_faults on macOS), and one table for all three left a
# macOS shard at twice the length of its sibling.
default_weight=3
case "${RUNNER_OS:-$(uname -s)}" in
macOS | Darwin) column=3 ;;
Windows | MINGW* | MSYS* | CYGWIN*) column=4 ;;
*) column=2 ;;
esac
weights="
class_completeness_corpus 1222 2064 4409
yaml_cases 266 957 915
impact 925 1527 1448
metamorphic 576 436 837
loss_sweep 699 525 789
unknowns 635 454 662
class_completeness 377 319 610
git_faults 257 464 0
occurrence_classes 108 170 245
hook 72 154 210
hook_property 124 171 0
lane_f 154 97 142
budget_contract 103 82 140
facts_cache 16 19 129
resolve_python 10 52 87
resolve_typescript 7 19 59
native_cli 38 30 51
deadline_answer 28 48 37
untracked_budget 7 23 46
resolve_java 2 11 32
bindings 16 24 30
resolve_kotlin 2 10 28
lane_ts 20 17 27
staged 15 12 27
resolve_rust 3 7 25
tests_same_file 14 13 22
lane_py 11 15 18
kotlin 13 12 17
lane_rs 11 10 16
lane_e1 13 14 14
resolve_go 1 6 14
fragment_snapshot 7 13 10
resolve_php 2 5 13
occurrence_outcome 1 6 11
test_claim 7 9 11
lane_sh 7 6 10
lib 5 9 10
occurrence_debug 6 5 10
data_file_added 9 6 9
edge_precision 4 5 9
git_processes 2 3 9
resolve_c 1 3 8
resolve_cpp 1 3 8
resolve_terraform 1 2 8
lane_swift 5 5 7
facts_changes 0 5 6
gap_reasons 2 4 5
module_graph 3 4 5
orphan_children 2 5 0
"

# The names on stdin that fall to shard $1 of $2, one per line.
shard_of() {
  {
    echo "$weights"
    cat
  } | awk -v fallback="$default_weight" -v column="$column" '
    NF == 4 { seconds[$1] = $column; next }
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

corpus=(class_completeness_corpus yaml_cases)

status=0
run() { "$@" || status=1; }

if ((run_rest)); then
  # --features python, or analytics/, graph_export/ and project_graph/ are not
  # compiled at all: they are cfg-gated on it and ship inside the wheel.
  names=$(
    {
      echo lib
      for file in tests/*.rs; do basename "$file" .rs; done | grep -vxF -e "${corpus[0]}" -e "${corpus[1]}"
    } | shard_of "$shard" "$shards"
  )
  suites=()
  for name in $names; do
    if [[ "$name" = lib ]]; then
      run cargo test --locked --lib --features python
      for crate in \
        diffctx-base \
        diffctx-config \
        diffctx-snapshot \
        diffctx-facts \
        diffctx-resolve \
        diffctx-occurrence \
        diffctx-graph \
        diffctx-select \
        diffctx-answer; do
        run cargo test --locked --package "$crate"
      done
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
  suites=()
  for name in $(printf '%s\n' "${corpus[@]}" | shard_of "$shard" "$shards"); do
    suites+=(--test "$name")
  done
  if ((${#suites[@]})); then
    run cargo test --release --locked --no-fail-fast "${suites[@]}"
  fi
fi
exit "$status"
