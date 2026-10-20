#!/usr/bin/env bash
# Prints every release tag that is not an ancestor of $1 (default origin/main),
# one per line. A consumer pins the commit a tag first named; a tag moved off
# main, or a rewrite that strands it, leaves that pin to garbage collection
# (#500, #513). Run from a clone with full history and tags.
set -euo pipefail
main=${1:-origin/main}
git tag -l 'v*' | while read -r tag; do
  git merge-base --is-ancestor "${tag}^{commit}" "$main" || echo "$tag"
done
