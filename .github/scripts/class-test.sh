#!/usr/bin/env bash
# The class-test gate (T9). A pull request that carries a `fix` commit touching
# engine source (crates/*/src, src) must carry the test that would have caught
# the fix's class: an added or changed case under tests/cases/{diff,impact,symbol}
# or the metamorphic, fault or hook-property suite. That test must FAIL on the
# pull request's base and PASS on its head, or any unrelated test change would
# satisfy the gate. The escape is a reviewed line in the PR body:
#   Class test: none — truth|leak|delivery gh#N
#
# Where each kind of test runs:
#   impact and symbol cases  the head's harness against a diffctx binary built
#                            from the base (DIFFCTX_BIN): the engine is a process
#   selection (diff) cases   in-process, so the base's own harness with only the
#                            pull request's case files laid over it
#   suites                   the base with the pull request's tests laid over it
# A test is red on the base only when its own assertion fails: a compile error,
# a crash, a missing binary or a harness setup error is an error of the gate
# (exit 2), never red. A case's assertion opens its failure with the line the
# harness defines as ASSERTION_FAILED (tests/common/oracle/mod.rs); a suite's is
# a failed test result.
#
# Needs BASE_SHA and HEAD_SHA (history of both), PR_BODY, cargo, and a checkout
# of HEAD_SHA as the working directory. BASE_TARGET_DIR is the cargo target
# directory of the base build; CI caches it by BASE_SHA.
set -euo pipefail
: "${BASE_SHA:?}" "${HEAD_SHA:?}"
PR_BODY=${PR_BODY:-}

crate=crates/diffctx-native
engine='^(crates/[^/]+/src/|src/)'
suites='^crates/[^/]+/tests/(metamorphic|git_faults|hook_property)\.rs$'
cases='^tests/cases/(diff|impact|symbol)/.+\.ya?ml$'
suite_overlay='^(crates/[^/]+/tests/|tests/cases/)|^crates/[^/]+/Cargo\.toml$|^Cargo\.lock$'
selection_cases='^tests/cases/diff/'
assertion='diffctx-case: assertion failed'
suite_assertion='test result: FAILED'
exemption='^[[:space:]]*Class test:[[:space:]]*none[[:space:]]*(—|–|--|-)[[:space:]]*(truth|leak|delivery)[[:space:]]+(gh|fj)#[0-9]+'

fix_touches_engine() {
  local sha files
  for sha in $(git rev-list --no-merges "${BASE_SHA}..${HEAD_SHA}"); do
    [[ "$(git log -1 --format=%s "$sha")" =~ ^fix(\([^\)]*\))?!?: ]] || continue
    files=$(git diff-tree --no-commit-id --name-only -r "$sha")
    if grep -Eq "$engine" <<<"$files"; then return 0; fi
  done
  return 1
}

if ! fix_touches_engine; then
  echo "class-test: no fix commit touches engine source; nothing to gate."
  exit 0
fi

if grep -Eq "$exemption" <<<"$PR_BODY"; then
  echo "class-test: the PR body carries a reviewed 'Class test: none' line; the orchestrator reads it."
  exit 0
fi

changes=$(git diff --name-status --no-renames "${BASE_SHA}...${HEAD_SHA}")
class_cases=$(awk -F'\t' '$1 != "D" {print $2}' <<<"$changes" | grep -E "$cases" || true)
class_suites=$(awk -F'\t' '$1 != "D" {print $2}' <<<"$changes" | grep -E "$suites" || true)
if [[ -z "$class_cases$class_suites" ]]; then
  echo "::error::a fix commit touches engine source and no class test was added or changed" \
    "(tests/cases/{diff,impact,symbol}/, metamorphic.rs, git_faults.rs, hook_property.rs)." \
    "Add one, or write 'Class test: none — truth|leak|delivery gh#N' in the PR body."
  exit 1
fi

work=${CLASS_TEST_WORK:-$(mktemp -d)}
mkdir -p "$work"
work=$(cd "$work" && pwd)
base_target=${BASE_TARGET_DIR:-${work}/base-target}
base_bin="${base_target}/release/diffctx"

trees=()
add_tree() { # name pattern: the base, with the pull request's files matching pattern laid over it
  local dir="${work}/$1" status path
  git worktree add --quiet --detach "$dir" "$BASE_SHA"
  trees+=("$dir")
  while IFS=$'\t' read -r status path; do
    [[ "$path" =~ $2 ]] || continue
    if [[ "$status" == D ]]; then
      rm -f "${dir}/${path}"
    else
      mkdir -p "$(dirname "${dir}/${path}")"
      git show "${HEAD_SHA}:${path}" >"${dir}/${path}"
    fi
  done <<<"$changes"
}
cleanup() {
  local dir
  for dir in ${trees[@]+"${trees[@]}"}; do git worktree remove --force "$dir" >/dev/null 2>&1 || true; done
}
trap cleanup EXIT

export DIFFCTX_YAML_IGNORE_BASELINE=1
# The tree's own cargo target stays the head's; the base keeps one of its own.
head_cargo() { (cd "$crate" && cargo test --release --locked "$@"); }
base_cargo() {
  local dir="${work}/$1"
  shift
  (cd "${dir}/${crate}" && CARGO_TARGET_DIR="$base_target" cargo test --release --locked "$@")
}
head_against_base_binary() { (cd "$crate" && DIFFCTX_BIN="$base_bin" cargo test --release --locked "$@"); }

selection_cases_changed=$(grep -E "$selection_cases" <<<"$class_cases" || true)
process_cases_changed=$(grep -Ev "$selection_cases" <<<"$class_cases" || true)

gate_errors=0
gate_error() {
  echo "::error::$*"
  gate_errors=$((gate_errors + 1))
}

suite_args=()
for path in $class_suites; do
  suite_args+=(--test "$(basename "$path" .rs)")
done
case_args=()
if [[ -n "$class_cases" ]]; then case_args+=(--test yaml_cases); fi

if [[ -n "$class_suites" ]]; then
  add_tree base-suites "$suite_overlay"
  if ! base_cargo base-suites --no-run "${suite_args[@]}"; then
    echo "::error::the class test does not compile against the base; it may use only what the base already has"
    exit 1
  fi
fi
if [[ -n "$selection_cases_changed" || -n "$process_cases_changed" ]]; then
  add_tree base "$selection_cases"
fi
if [[ -n "$selection_cases_changed" ]] && ! base_cargo base --no-run --test yaml_cases; then
  echo "::error::the base's own harness does not build"
  exit 2
fi
if [[ -n "$process_cases_changed" ]]; then
  if ! (cd "${work}/base/${crate}" && CARGO_TARGET_DIR="$base_target" cargo build --release --locked --bin diffctx); then
    echo "::error::the base does not build its binary"
    exit 2
  fi
  if [[ ! -x "$base_bin" ]]; then
    echo "::error::the base build left no binary at ${base_bin}"
    exit 2
  fi
fi
head_cargo --no-run ${suite_args[@]+"${suite_args[@]}"} ${case_args[@]+"${case_args[@]}"}

failures=0
expect() { # side expected-verdict label assertion-marker command...
  local side=$1 want=$2 label=$3 marker=$4 got=0 output verdict
  shift 4
  output=$("$@" 2>&1) || got=$?
  if [[ "$got" -eq 0 ]]; then
    verdict=green
  elif grep -qF "$marker" <<<"$output"; then
    verdict=red
  else
    printf '%s\n' "$output" | tail -n 40
    gate_error "${label}: exit ${got} on the ${side} without a failed assertion: a crash, a missing binary or a setup error, not a red test"
    return 0
  fi
  if [[ "$verdict" != "$want" ]]; then
    printf '%s\n' "$output" | tail -n 40
    echo "::error::${label}: expected ${want} on the ${side}, exit ${got}"
    failures=$((failures + 1))
  else
    echo "class-test: ${label} is ${want} on the ${side}"
  fi
}

for path in $class_suites; do
  name=$(basename "$path" .rs)
  expect base red "suite ${name}" "$suite_assertion" base_cargo base-suites --test "$name"
  expect head green "suite ${name}" "$suite_assertion" head_cargo --test "$name"
done

if [[ -n "$class_cases" ]]; then
  listing=$(head_cargo --test yaml_cases -- --list 2>/dev/null)
  for path in $class_cases; do
    rel=${path#tests/cases/}
    rel=${rel%.*}
    trial=
    for candidate in "$rel" "${rel#*/}"; do
      if grep -qxF "[yaml] ${candidate}: test" <<<"$listing"; then
        trial=$candidate
        break
      fi
    done
    if [[ -z "$trial" ]]; then
      echo "::error::${path}: the harness lists no trial for it"
      failures=$((failures + 1))
      continue
    fi
    if [[ "$path" =~ $selection_cases ]]; then
      expect base red "case ${trial}" "$assertion" base_cargo base --test yaml_cases -- --exact "$trial"
    else
      expect base red "case ${trial}" "$assertion" head_against_base_binary --test yaml_cases -- --exact "$trial"
    fi
    expect head green "case ${trial}" "$assertion" head_cargo --test yaml_cases -- --exact "$trial"
  done
fi

[[ "$gate_errors" -eq 0 ]] || exit 2
[[ "$failures" -eq 0 ]]
