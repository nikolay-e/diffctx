#!/usr/bin/env bash
# Prints every release tag (v*) that has no published GitHub Release, one per
# line. A drafted release 404s for everyone but a writer while the tag still
# looks released (#513); a token that cannot see drafts lists them as absent,
# which is the same verdict. Needs REPO, a gh login and a clone with tags.
set -euo pipefail
: "${REPO:?}"
published=$(gh release list --repo "$REPO" --limit 1000 --json tagName,isDraft --jq '.[] | select(.isDraft | not) | .tagName')
git tag -l 'v*' | while read -r tag; do
  grep -qxF "$tag" <<<"$published" || echo "$tag"
done
