#!/usr/bin/env bash
# The suites that build git repositories, run under the environment a pre-commit hook gives a
# linked worktree, against a sentinel repository: one `git init` that inherited it rewrote the
# maintainer checkout's .git/config (core.bare = true, fj#199). The gate fails when any file of
# the sentinel's .git (objects aside) differs afterwards.
#
#   scripts/git-env-gate.sh [rust|python|all] [-- extra args for the runner]
#
# It calls cargo and pytest directly, not scripts/cargo-test-suites.sh, which scrubs the
# environment itself: the gate is what shows a spawn that does not.
# GATE_PYTHON names the interpreter (default python3).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
# shellcheck source=scripts/repo-locating-env.sh
source "$root/scripts/repo-locating-env.sh"
unset "${REPO_LOCATING_ENV[@]}"

part=${1:-all}
[[ $# -gt 0 ]] && shift
[[ ${1:-} == -- ]] && shift
case "$part" in rust | python | all) ;; *)
  echo "usage: $0 [rust|python|all] [-- args]" >&2
  exit 2
  ;;
esac

sentinel=$(mktemp -d)
trap 'rm -rf "$sentinel"' EXIT
git init -q -b main "$sentinel/repo"
git -C "$sentinel/repo" -c user.name=gate -c user.email=gate@example.invalid commit -q --allow-empty -m sentinel
git -C "$sentinel/repo" worktree add -q "$sentinel/worktree" -b gate

gitdir=$sentinel/repo/.git/worktrees/worktree
state() {
  (
    cd "$sentinel/repo/.git"
    find . -path ./objects -prune -o -type f ! -name '*.lock' -print | LC_ALL=C sort |
      while IFS= read -r file; do
        printf '%s  %s\n' "$(shasum -a 256 <"$file" | cut -d' ' -f1)" "$file"
      done
  )
}
before=$(state)
cp "$sentinel/repo/.git/config" "$sentinel/config.before"

in_hook_env() {
  env GIT_DIR="$gitdir" GIT_INDEX_FILE="$gitdir/next-index-1.lock" GIT_PREFIX= "$@"
}

status=0
if [[ $part != python ]]; then
  cd "$root/crates/diffctx-native"
  suites=(--lib --test staged --test snapshot_hook_env --test hook)
  [[ $# -gt 0 && $part == rust ]] && suites=("$@")
  in_hook_env cargo test "${suites[@]}" || status=1
fi
if [[ $part != rust ]]; then
  cd "$root"
  tests=(tests/test_fixture_git_env.py tests/test_epoch_push.py tests/test_release_tags.py tests/test_class_test_gate.py)
  [[ $# -gt 0 && $part == python ]] && tests=("$@")
  in_hook_env "${GATE_PYTHON:-python3}" -I -m pytest -q -p no:cacheprovider "${tests[@]}" || status=1
fi

after=$(state)
if [[ $before != "$after" ]]; then
  echo "git-env-gate: a suite wrote into the enclosing repository:" >&2
  diff "$sentinel/config.before" "$sentinel/repo/.git/config" >&2 || true
  diff <(echo "$before") <(echo "$after") >&2 || true
  exit 1
fi
[[ $status -eq 0 ]] || echo "git-env-gate: the sentinel is intact, but a suite failed" >&2
exit "$status"
