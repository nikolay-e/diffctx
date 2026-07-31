#!/usr/bin/env bash
#
# Bit-equivalence check for E-class changes, run against this repo itself.
#
# The project's rule is that a refactor claiming "no output change" proves it.
# That proof already existed as a habit — changelog entries say "byte-identical
# across 4 scoring modes x 3 diff ranges" — but a habit cannot be required by
# review, and the eval-run gate (`python -m eval equivalence`) needs two full
# eval output directories, which is far too heavy to run per commit.
#
# Usage:
#     scripts/bitcheck.sh record      # snapshot the CURRENT build
#     <edit code, rebuild>
#     scripts/bitcheck.sh check       # fail if any cell moved
#     scripts/bitcheck.sh clean       # drop the snapshots and the fixture worktree
#
# Snapshots live under a scratch dir, never in git. `latency` is stripped before
# comparison: it is wall-clock, so it differs run to run even on identical code.
#
# Exit 0 = every cell identical. Exit 1 = a cell moved, with the diff printed.
set -euo pipefail

REPO_ROOT="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"
SNAP_DIR="${BITCHECK_DIR:-${TMPDIR:-/tmp}}/diffctx-bitcheck"

# The INPUT is a frozen worktree, not the live checkout. diffctx analysing its
# own repository means its source files appear in its own output as context
# fragments — so editing the code being tested also edits the input, and the
# baseline goes stale for a reason that has nothing to do with behaviour. The
# first version of this script ran against the live tree and reported 8 moved
# cells for a change that could not affect them: stashing one file had rewritten
# a context fragment. Pinning the worktree makes the binary the only variable.
FIXTURE_SHA="5e2025ab"
FIXTURE="$SNAP_DIR/fixture"

ensure_fixture() {
  # The fixture lives under TMPDIR, which the OS clears without telling git.
  # Without this prune the stale registration survives and `worktree add`
  # refuses the path.
  git -C "$REPO_ROOT" worktree prune
  if [[ -f "$FIXTURE/Cargo.toml" ]]; then
    return
  fi
  mkdir -p "$SNAP_DIR"
  rm -rf "$FIXTURE"
  git -C "$REPO_ROOT" worktree add --detach -q "$FIXTURE" "$FIXTURE_SHA"
  printf 'fixture worktree at %s (%s)\n' "$FIXTURE" "$FIXTURE_SHA"
}

# Fixed SHAs, not relative refs: `HEAD~20` names a different range every time a
# commit lands, which would make a "moved" cell unreadable. These are chosen for
# shape, not size — a wide multi-language range, a narrow one, and a
# rename/deletion-carrying one, so fragmentation, scoping and git plumbing are
# all exercised.
RANGES=(
  "33731da6..5e2025ab" # 68 files, Rust + Python + docs, today's batch
  "48672615..e5540835" # single commit, one Rust module
  "c6ae1b04..d4028021" # corpus schema + provenance, deletions and renames
)
MODES=(ego ppr bm25 rrf)
# `locate` is a separate public schema (`locate.v1`) built from the same
# selection, so a change can leave pack output alone and still move it — the
# grouping and blast-radius summary are computed only on this path.
KINDS=(pack locate)
# The eval default. Realistic pressure, and every cell finishes in seconds.
BUDGET="${BITCHECK_BUDGET:-8000}"

CELL_TOTAL=$((${#RANGES[@]} * ${#MODES[@]} * ${#KINDS[@]}))

cell_name() {
  local range="$1" mode="$2" kind="$3"
  # `/` and `.` in a range make poor filenames.
  printf '%s__%s__%s' "${range//[^a-zA-Z0-9]/_}" "$mode" "$kind"
}

run_cell() {
  local range="$1" mode="$2" kind="$3" out="$4"
  # The native binary, not the Python CLI: fewer layers between the change and
  # the artifact.
  #
  # A FINITE budget on purpose. `--budget -1` looks like the stronger check —
  # no budget pressure, so the comparison sees fragmentation rather than where
  # the greedy stopped — but selection plus the post-passes are ~97% of wall
  # clock on a wide range (#121), and unlimited means the greedy never stops
  # early: every cell blew the 300s deadline. A budget that binds also
  # exercises admission order, which is where most output changes show up.
  #
  # stderr is NOT discarded. Hiding it is how the first version of this script
  # reported "recording 24 cells" and then wrote an empty directory.
  if ! "$REPO_ROOT/target/release/diffctx" "$FIXTURE" \
    --diff "$range" --scoring "$mode" --mode "$kind" \
    --budget "$BUDGET" --format json --quiet \
    >"$out.raw"; then
    printf 'cell failed: %s %s %s\n' "$range" "$mode" "$kind" >&2
    exit 3
  fi
  if [[ ! -s "$out.raw" ]]; then
    printf 'cell produced no output: %s %s %s\n' "$range" "$mode" "$kind" >&2
    exit 3
  fi
  python3 -c '
import json, sys
doc = json.load(open(sys.argv[1]))
# Wall-clock fields are not part of the contract being checked.
doc.pop("latency", None)
json.dump(doc, open(sys.argv[2], "w"), indent=1, sort_keys=True)
' "$out.raw" "$out"
  rm -f "$out.raw"
}

sweep() {
  local dest="$1"
  ensure_fixture
  rm -rf "$dest"
  mkdir -p "$dest"
  for range in "${RANGES[@]}"; do
    for mode in "${MODES[@]}"; do
      for kind in "${KINDS[@]}"; do
        local name
        name="$(cell_name "$range" "$mode" "$kind")"
        run_cell "$range" "$mode" "$kind" "$dest/$name.json"
        printf '  %s\n' "$name"
      done
    done
  done
}

case "${1:-}" in
record)
  printf 'recording %d cells from the current build\n' "$CELL_TOTAL"
  sweep "$SNAP_DIR/base"
  printf 'baseline at %s\n' "$SNAP_DIR/base"
  ;;
check)
  if [[ ! -d "$SNAP_DIR/base" ]]; then
    printf 'no baseline: run scripts/bitcheck.sh record on the pre-change build first\n' >&2
    exit 2
  fi
  printf 'checking against %s\n' "$SNAP_DIR/base"
  sweep "$SNAP_DIR/new"
  moved=0
  for f in "$SNAP_DIR/base"/*.json; do
    name="$(basename "$f")"
    if ! diff -q "$f" "$SNAP_DIR/new/$name" >/dev/null 2>&1; then
      moved=$((moved + 1))
      printf '\nMOVED %s\n' "$name"
      diff -u "$f" "$SNAP_DIR/new/$name" | head -40 || true
    fi
  done
  if ((moved > 0)); then
    printf '\n%d of %d cells moved — this change is NOT bit-equivalent\n' \
      "$moved" "$CELL_TOTAL" >&2
    exit 1
  fi
  printf '\nall %d cells identical\n' "$CELL_TOTAL"
  ;;
clean)
  # Leaves no registration behind: the fixture is a real worktree, and a
  # dangling entry outlives the directory TMPDIR reclaims.
  git -C "$REPO_ROOT" worktree remove --force "$FIXTURE" 2>/dev/null || true
  git -C "$REPO_ROOT" worktree prune
  rm -rf "$SNAP_DIR"
  printf 'removed %s and pruned the fixture worktree\n' "$SNAP_DIR"
  ;;
*)
  sed -n '3,18p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  exit 2
  ;;
esac
