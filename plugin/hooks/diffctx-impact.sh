#!/usr/bin/env bash
# PreToolUse / PostToolUse: hands the tool event on stdin to `diffctx hook
# <event>` from the diffctx package pinned below, which prints the change's
# impact as additionalContext or nothing. It is the same pinned uvx launch as
# the MCP server, so the environment the server's start put in uv's cache is
# the one this runs; --offline keeps the commit path off the network. Every
# path ends in exit 0: a non-zero exit would block the agent's git command,
# and no impact review is worth a blocked commit.
set -u

[[ "${CLAUDE_PLUGIN_OPTION_IMPACT_HOOK:-true}" == "true" ]] || exit 0

# Read the event once and leave every other Bash call alone before starting
# anything: the matcher is every Bash call, the interest is a few.
payload=$(cat 2>/dev/null) || exit 0
case "$payload" in
*'git '* | *'gh '*) ;;
*) exit 0 ;;
esac
case "$payload" in
*'"PreToolUse"'*) event=pretooluse ;;
*'"PostToolUse"'*) event=posttooluse ;;
*) exit 0 ;;
esac

# One array, never empty: under bash 3.2 (macOS /bin/bash) `set -u` treats
# an empty array's expansion as unbound and the pipeline dies silently.
args=(hook "$event")
if [[ "${CLAUDE_PLUGIN_OPTION_IMPACT_GATE:-false}" == "true" ]]; then
  args+=(--gate)
fi

if [[ -n "${DIFFCTX_HOOK_BIN:-}" ]]; then
  launch=("$DIFFCTX_HOOK_BIN")
else
  command -v uvx >/dev/null 2>&1 || exit 0
  launch=(uvx -q --offline -c "${CLAUDE_PLUGIN_ROOT:-.}/constraints.txt" "diffctx[mcp]==1.18.1")
fi

# A release without the subcommand, or a cache uv has not filled yet, exits
# non-zero and prints nothing, which is the right answer on the commit path.
printf '%s' "$payload" | "${launch[@]}" "${args[@]}" 2>/dev/null || true
exit 0
