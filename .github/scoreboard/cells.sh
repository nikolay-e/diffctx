# perf.sh and gitcount.sh measure the same cells on the same trees; both source this with FIX set.
# shellcheck shell=bash disable=SC2034
cells=(
  "impact-narrow|--diff 384c4b86..3562b922 --mode impact"
  "pack-ego-wide|--diff 0d75e73a..99ff4abc --scoring ego --mode pack --budget 8000"
  "locate-ppr-narrow|--diff 384c4b86..3562b922 --scoring ppr --mode locate --budget 8000"
  "symbol|--symbol build_diff_context"
)
# 1.20.0 reads the ignore rules of the working tree, the head those of the range's end: on a fixture checked out at
# another commit the two read different file sets, so each --diff cell runs on a tree checked out at its range end.
tree_for() {
  local end
  [[ $1 =~ --diff\ [^\ ]*\.\.([^\ ]+) ]] || {
    echo "$FIX"
    return
  }
  end=${BASH_REMATCH[1]}
  [ -d "$FIX-$end" ] || git -C "$FIX" worktree add -q --detach "$FIX-$end" "$end" >&2
  echo "$FIX-$end"
}
