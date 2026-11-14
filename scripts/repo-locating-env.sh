# shellcheck shell=bash
# The variables that tell git which repository, work tree, index or object store to use. A git
# hook exports them (from a linked worktree GIT_DIR names the shared repository's worktree
# directory); a child that inherits them reads and writes the hook's repository: `git init` in
# a temporary directory then rewrites the shared .git/config (core.bare = true, fj#199).
# crates/diffctx-native/src/snapshot/git.rs and tests/conftest.py carry the same list.
# shellcheck disable=SC2034  # read by the scripts that source this file
REPO_LOCATING_ENV=(
  GIT_DIR
  GIT_WORK_TREE
  GIT_INDEX_FILE
  GIT_COMMON_DIR
  GIT_OBJECT_DIRECTORY
  GIT_ALTERNATE_OBJECT_DIRECTORIES
  GIT_PREFIX
)
