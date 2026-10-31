#!/usr/bin/env bash
# The class-test gate (T9). A pull request that carries a `fix` commit touching
# engine source (crates/*/src, src) must carry the test that would have caught
# the fix's class: an added or changed case under tests/cases/{diff,impact,symbol}
# or the metamorphic, fault or hook-property suite. That test must FAIL on the
# pull request's base and PASS on its head, or any unrelated test change would
# satisfy the gate. The escape is a reviewed line in the PR body:
#   Class test: none — truth|leak|delivery gh#N
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
overlay='^(crates/[^/]+/tests/|tests/cases/)|^crates/[^/]+/Cargo\.toml$|^Cargo\.lock$'
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
base="${work}/base"
trap 'git worktree remove --force "$base" >/dev/null 2>&1 || true' EXIT
git worktree add --quiet --detach "$base" "$BASE_SHA"

# The class test and the harness that reads it are the pull request's; the
# engine under it is the base's.
while IFS=$'\t' read -r status path; do
  [[ "$path" =~ $overlay ]] || continue
  if [[ "$status" == D ]]; then
    rm -f "${base}/${path}"
  else
    mkdir -p "$(dirname "${base}/${path}")"
    git show "${HEAD_SHA}:${path}" >"${base}/${path}"
  fi
done <<<"$changes"

export DIFFCTX_YAML_IGNORE_BASELINE=1
# The tree's own cargo target stays the head's; the base keeps one of its own.
head_cargo() { (cd "$crate" && cargo test --release --locked "$@"); }
base_cargo() { (cd "${base}/${crate}" && CARGO_TARGET_DIR="${BASE_TARGET_DIR:-${work}/base-target}" cargo test --release --locked "$@"); }

suite_args=()
for path in $class_suites; do
  name=$(basename "$path" .rs)
  suite_args+=(--test "$name")
done
if [[ -n "$class_cases" ]]; then suite_args+=(--test yaml_cases); fi

if ! base_cargo --no-run "${suite_args[@]}"; then
  echo "::error::the class test does not compile against the base; it may use only what the base already has"
  exit 1
fi
head_cargo --no-run "${suite_args[@]}"

failures=0
expect() { # side expected-exit label command...
  local side=$1 want=$2 label=$3 got=0 output
  shift 3
  output=$("$@" 2>&1) || got=$?
  if [[ ("$want" == red && "$got" -eq 0) || ("$want" == green && "$got" -ne 0) ]]; then
    printf '%s\n' "$output" | tail -n 40
    echo "::error::${label}: expected ${want} on the ${side}, exit ${got}"
    failures=$((failures + 1))
  else
    echo "class-test: ${label} is ${want} on the ${side}"
  fi
}

for path in $class_suites; do
  name=$(basename "$path" .rs)
  expect base red "suite ${name}" base_cargo --test "$name"
  expect head green "suite ${name}" head_cargo --test "$name"
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
    expect base red "case ${trial}" base_cargo --test yaml_cases -- --exact "$trial"
    expect head green "case ${trial}" head_cargo --test yaml_cases -- --exact "$trial"
  done
fi

[[ "$failures" -eq 0 ]]
