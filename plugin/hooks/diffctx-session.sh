#!/usr/bin/env bash
# SessionStart: fills uv's cache with the pinned diffctx package the git
# hooks run offline, in the background so the session never waits on it,
# and puts one line of standing context into the session: a command to run
# rather than a tool to consider, since models follow a concrete recipe more
# readily. Nothing is downloaded but the pinned package. Every path ends in
# exit 0.
set -u

# Up to 1.18.1 this hook downloaded a binary per release into the plugin's
# data directory. Nothing reads it since, and an update never removed the
# previous one: 61 MB per version left behind (#52).
if [[ -n "${CLAUDE_PLUGIN_DATA:-}" ]]; then
  rm -rf "${CLAUDE_PLUGIN_DATA:?}/bin" "${CLAUDE_PLUGIN_DATA:?}"/install-failed-* 2>/dev/null
fi

[[ "${CLAUDE_PLUGIN_OPTION_IMPACT_HOOK:-true}" == "true" ]] || exit 0
command -v uvx >/dev/null 2>&1 || exit 0

(uvx -q -c "${CLAUDE_PLUGIN_ROOT:-.}/constraints.txt" "diffctx[mcp]==1.18.2" --version \
  </dev/null >/dev/null 2>&1 &)

printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"diffctx is installed. Before committing or pushing a multi-file change, run: uvx diffctx==1.18.2 . --diff --mode impact -f md  (what the change reaches outside its diff: callers, their tests, cross-commit overlap). The plugin also injects this before git commit/merge/push and after git diff."}}'
exit 0
