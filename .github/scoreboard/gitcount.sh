#!/usr/bin/env bash
# git processes each binary starts per cell (perf.sh's cells), counted from execve under strace: the second half of
# the "not slower than 1.20.0" gate, which callgrind cannot see because git children are not traced there.
# Usage: gitcount.sh <old-bin> <new-bin> <fixture> <out-dir>   Prints a table; never fails.
set -uo pipefail
O=$1
N=$2
FIX=$3
OUT=$4
mkdir -p "$OUT"
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
count() {
  local bin=$1 tag=$2 name=$3 args=$4 tree=$5
  # shellcheck disable=SC2086
  DIFFCTX_TOKEN_CACHE_DIR="$OUT/cache-$tag" "$bin" "$tree" $args --timeout 86400 --format json --quiet >/dev/null 2>&1
  # shellcheck disable=SC2086
  DIFFCTX_TOKEN_CACHE_DIR="$OUT/cache-$tag" strace -f -qq -e trace=execve -e signal=none -o "$OUT/$name-$tag.st" \
    "$bin" "$tree" $args --timeout 86400 --format json --quiet >/dev/null 2>&1
  grep -cE 'execve\("[^"]*/git", .*= 0$' "$OUT/$name-$tag.st"
}
echo "| cell | 1.20.0 git processes | head git processes |"
echo "|---|---:|---:|"
for cell in "${cells[@]}"; do
  name=${cell%%|*}
  args=${cell#*|}
  tree=$(tree_for "$args")
  echo "| $name | $(count "$O" old "$name" "$args" "$tree") | $(count "$N" new "$name" "$args" "$tree") |"
done
