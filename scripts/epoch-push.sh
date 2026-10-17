#!/usr/bin/env bash
# The maintainers' work reaches `main` one epoch at a time. Between landings it is a single
# commit on `next`, on top of `main`, so no branch piles up commits and `main` is never
# rewritten: forks, contributors' pull requests and release tags all keep naming real commits.
#
#   scripts/epoch-push.sh        fold every commit since main into the epoch commit,
#                                push it to `next` on every remote, open the epoch PR
#   scripts/epoch-push.sh land   fast-forward `main` to the epoch commit on every remote
set -euo pipefail

die() {
  echo "epoch-push: $*" >&2
  exit 1
}

[ "$(git symbolic-ref --short HEAD 2>/dev/null)" = next ] || die "run it on the next branch"
[ -z "$(git status --porcelain --untracked-files=no)" ] || die "commit or stash tracked changes first"

src=origin
remotes=$(git remote)
for r in $remotes; do git fetch --quiet "$r"; done
has_next() { git rev-parse --verify --quiet "refs/remotes/$1/next" >/dev/null; }
# The epoch PR lives where CI runs; with two remotes, gh would otherwise stop to ask which.
gh_repo=$(git remote -v | sed -nE 's#.*github\.com[:/]([^ ]+/[^ ]+) \(push\)$#\1#p' | sed 's/\.git$//' | head -1)
[ -n "$gh_repo" ] && command -v gh >/dev/null || gh_repo=''
epoch_pr() { gh pr list -R "$gh_repo" --head next --base main --state open --json number -q '.[0].number // empty'; }

fold() {
  # Another session may have folded its work since our last fetch: replay only our own
  # commits onto its epoch (the fork point follows the rewritten `next`), then onto `main`,
  # which contributors' pull requests and dependency bumps move underneath the epoch.
  if has_next "$src"; then git rebase --quiet --fork-point "$src/next"; fi
  git rebase --quiet "$src/main"

  local commits oldest msg tree epoch
  commits=$(git rev-list --reverse "$src/main..HEAD")
  [ -n "$commits" ] || {
    echo "epoch-push: nothing above main"
    return
  }
  # A contributor's commit is theirs: it lands on main as itself, never inside an epoch.
  local me foreign
  me=$(git config user.email)
  foreign=$(git log --format='%h %an <%ae>' "$src/main..HEAD" | grep -vF "<$me>" || true)
  [ -z "$foreign" ] || die "commits by someone else sit above main — land them on main as themselves first:
$foreign"
  oldest=$(printf '%s\n' "$commits" | head -1)
  msg=$(mktemp)
  trap 'rm -f "$msg"' RETURN
  {
    git log -1 --format=%B "$oldest"
    for c in $(printf '%s\n' "$commits" | tail -n +2); do
      echo "- $(git log -1 --format=%s "$c")"
      git log -1 --format=%B "$c" | grep -iE '^(fixes|closes|resolves) #[0-9]+' || true
    done
  } | git stripspace >"$msg"

  # The tree is the one every folded commit already took through pre-commit; the hooks a
  # plain `git commit` runs afterwards still run, so the repository's own policies apply.
  git hook run --ignore-missing commit-msg -- "$msg"
  tree=$(git rev-parse 'HEAD^{tree}')
  epoch=$(git commit-tree "$tree" -p "$src/main" -F "$msg")
  git reset --quiet --soft "$epoch"
  git hook run --ignore-missing post-commit

  for r in $remotes; do git push --quiet --force-with-lease=refs/heads/next "$r" HEAD:refs/heads/next; done
  if [ -n "$gh_repo" ] && [ -z "$(epoch_pr)" ]; then
    gh pr create -R "$gh_repo" --draft --base main --head next --title "$(git log -1 --format=%s)" \
      --body "One epoch of maintainer work, kept as a single commit by scripts/epoch-push.sh. It lands on main by fast-forward."
  fi
  echo "epoch-push: next = main + $(git log -1 --format='%h %s')"
}

land() {
  [ "$(git rev-parse HEAD)" = "$(git rev-parse "$src/next")" ] || die "local next differs from $src/next; fold first"
  [ "$(git rev-list --count "$src/main..$src/next")" = 1 ] || die "$src/next is not one commit above $src/main; fold first"
  if [ -n "$gh_repo" ]; then
    pr=$(epoch_pr)
    [ -z "$pr" ] || gh pr checks -R "$gh_repo" "$pr" >/dev/null || die "the epoch PR #$pr is not green yet"
  fi
  for r in $remotes; do git push --quiet "$r" "HEAD:refs/heads/main"; done
  echo "epoch-push: main = $(git log -1 --format='%h %s')"
}

case "${1:-}" in
"") fold ;;
land) land ;;
*) die "usage: scripts/epoch-push.sh [land]" ;;
esac
