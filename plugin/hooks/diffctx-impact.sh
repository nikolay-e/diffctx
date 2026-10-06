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
*'"PreToolUse"'*) event=pretooluse hook_event=PreToolUse ;;
*'"PostToolUse"'*) event=posttooluse hook_event=PostToolUse ;;
*) exit 0 ;;
esac

# After a text search the hook answers a grep for a name the pending change
# defines (#337). A search heads its statement (`… | grep` filters output),
# and a clean tree has nothing to answer with, so most searches end here.
# Only the command is read for a search or a `cd`: the payload also carries
# the tool's output, and a `cd /srv` the grep printed is not where it ran.
searches_pending_change() {
  local command='' re='"command"[[:space:]]*:[[:space:]]*"(([^"\\]|\\.)*)"'
  [[ $payload =~ $re ]] && command=${BASH_REMATCH[1]}
  re='"tool_name"[[:space:]]*:[[:space:]]*"Grep"'
  local search='(^|"|[;&(])[[:space:]]*(git[[:space:]]+grep|e?grep|fgrep|rg|ag|ack)[[:space:]]'
  [[ $payload =~ $re || $command =~ $search ]] || return 1
  local dir=.
  re='"cwd"[[:space:]]*:[[:space:]]*"([^"]*)"'
  [[ $payload =~ $re ]] && dir=${BASH_REMATCH[1]}
  re='(^|[[:space:]"])cd[[:space:]]+((/|~/)[^[:space:];&|"\\]+)'
  [[ $command =~ $re ]] && dir=${BASH_REMATCH[2]}
  # The payload's literal `~/`, expanded the way the shell running the command will.
  [[ ${dir:0:1} == "~" && ${dir:1:1} == / ]] && dir="${HOME:-}/${dir:2}"
  git -C "$dir" diff --quiet HEAD -- 2>/dev/null
  [[ $? -eq 1 ]]
}
case "$payload" in
*'git '* | *'gh '*) ;;
*) [[ $event == posttooluse ]] && searches_pending_change || exit 0 ;;
esac

# The binary logs every run it gets (#338); a launch that never reached it
# is logged here, or an audit could not tell it from a hook with nothing to say.
log_failure() {
  local dir="${DIFFCTX_CACHE_DIR:-${XDG_CACHE_HOME:-${HOME:-}/.cache}}/diffctx" session=-
  local re='"session_id"[[:space:]]*:[[:space:]]*"([A-Za-z0-9-]+)"'
  [[ $payload =~ $re ]] && session=${BASH_REMATCH[1]}
  mkdir -p "$dir" 2>/dev/null &&
    printf '%s %s %s - - error:%s - -\n' "$(date +%s)" "$session" "$hook_event" "$1" >>"$dir/hook.log" 2>/dev/null
  return 0
}

# One array, never empty: under bash 3.2 (macOS /bin/bash) `set -u` treats
# an empty array's expansion as unbound and the pipeline dies silently.
args=(hook "$event")
# Only a commit about to run can be gated; the other events take no flag.
if [[ $event == pretooluse && "${CLAUDE_PLUGIN_OPTION_IMPACT_GATE:-false}" == "true" ]]; then
  args+=(--gate)
fi

if [[ -n "${DIFFCTX_HOOK_BIN:-}" ]]; then
  launch=("$DIFFCTX_HOOK_BIN")
else
  command -v uvx >/dev/null 2>&1 || {
    log_failure no-uvx
    exit 0
  }
  launch=(uvx -q --offline -c "${CLAUDE_PLUGIN_ROOT:-.}/constraints.txt" "diffctx[mcp]==1.18.3")
fi

# A release without the subcommand, or a cache uv has not filled yet, exits
# non-zero and prints nothing, which is the right answer on the commit path.
printf '%s' "$payload" | "${launch[@]}" "${args[@]}" 2>/dev/null
status=$?
[[ $status -eq 0 ]] || log_failure "launch-$status"
exit 0
