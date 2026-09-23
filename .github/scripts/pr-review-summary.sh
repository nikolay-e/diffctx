#!/usr/bin/env bash
# Renders the sticky PR comment from a diffctx context file. Needs no token:
# the fork-PR path (read-only GITHUB_TOKEN) runs this and stops at the step
# summary, the same-repo path additionally posts the file it wrote.
set -euo pipefail

: "${CONTEXT_FILE:?}" "${TOKEN_COUNT:?}" "${DIFF_RANGE:?}" "${EMPTY:?}" "${RUN_URL:?}" "${OUT:?}"

marker='<!-- diffctx-review-context -->'
# The changed-files block: from its header to the first blank line after the
# list starts (the header is followed by one blank line).
changed=$(awk '/^\*\*Changed files:\*\*/{f=1;next}
               f&&/^$/{if(seen)exit; next}
               f{seen=1; print}' "$CONTEXT_FILE")
files=$(printf '%s\n' "$changed" | grep -c . || true)
frags=$(grep -cE '^## `' "$CONTEXT_FILE" || true)
# The first thing dogfooding found: this repo's own .diffctx/ignore withholds
# *.yml, so a PR whose main change is a workflow shows up with that file absent
# and nothing saying so. Surface the policy line the artifact carries, so the
# reviewer knows to open the raw diff for exactly those.
withheld=$(grep -oE '^\*[0-9]+ changed file\(s\) withheld by exclusion policy[^*]*\*' "$CONTEXT_FILE" || true)
{
  echo "$marker"
  echo "### diffctx review context"
  echo
  echo "Range \`${DIFF_RANGE}\` — **${TOKEN_COUNT} tokens** (o200k_base)," \
    "${frags} fragments across ${files} changed files. Empty: ${EMPTY}."
  echo
  echo "Review this instead of the raw diff: [download the context](${RUN_URL}#artifacts)" \
    "(artifact \`diffctx-review-context\`, 14 days)."
  if [[ -n "$withheld" ]]; then
    echo
    echo "${withheld} — read those in the raw diff."
  fi
  echo
  echo "<details><summary>Changed files as diffctx lists them (omitted = no fragment fit)</summary>"
  echo
  printf '%s\n' "$changed" | head -80
  echo
  echo "</details>"
} >"$OUT"
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  cat "$OUT" >>"$GITHUB_STEP_SUMMARY"
fi
echo "wrote $OUT (${frags} fragments, ${files} files)"
