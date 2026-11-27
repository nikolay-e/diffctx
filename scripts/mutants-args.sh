#!/usr/bin/env bash
# What .github/workflows/mutants.yml runs, one argument per line.
#   mutants-args.sh mutants <fast|heavy> <occurrence|answer>  the cargo-mutants arguments
#   mutants-args.sh tests <fast|heavy> <occurrence|answer>    the same suites as one plain `cargo test`
#   mutants-args.sh suites <fast|heavy>                       the integration suites of a phase
# The fast suites run on every mutant and are seconds each; the heavy ones are the case corpora, minutes.
# cargo-mutants builds with a bare `cargo test --no-run`, which compiles every test binary of the package
# (106 of them); the suites go in as --cargo-arg, so the build and the run name the same few targets.
set -euo pipefail

suites() {
  case "$1" in
  fast) printf '%s\n' occurrence_alias occurrence_classes occurrence_debug occurrence_implicit \
    occurrence_outcome occurrence_scan gap_reasons test_claim tests_same_file edge_precision data_file_added ;;
  heavy) printf '%s\n' class_completeness impact yaml_cases ;;
  *)
    echo "phase: fast or heavy" >&2
    exit 2
    ;;
  esac
}

mode=${1:?mode}
case "$mode" in
suites) suites "${2:?phase}" ;;
mutants | tests)
  phase=${2:?phase}
  pkg=diffctx-${3:?package}
  names=()
  while IFS= read -r name; do names+=("$name"); done < <(suites "$phase")
  if [ "$mode" = tests ]; then
    printf '%s\n' --package "$pkg" --package diffctx --lib
    for s in "${names[@]}"; do printf '%s\n' --test "$s"; done
    exit 0
  fi
  printf '%s\n' --package "$pkg"
  if [ "$3" = answer ]; then
    printf '%s\n' --file 'crates/diffctx-answer/src/decide/**'
  else
    # cfg(debug_assertions): absent from the release build, so its mutants can only survive
    printf '%s\n' --exclude 'crates/diffctx-occurrence/src/tokens/occurrence/identity.rs'
  fi
  printf '%s\n' --in-place --cargo-arg=--release --test-workspace=false --test-package "$pkg,diffctx" --cargo-arg=--lib
  for s in "${names[@]}"; do printf '%s\n' "--cargo-arg=--test" "--cargo-arg=$s"; done
  ;;
*)
  echo "usage: $0 mutants|tests <fast|heavy> <package> | suites <fast|heavy>" >&2
  exit 2
  ;;
esac
