#!/usr/bin/env bash
# PreToolUse / PostToolUse: hands the tool event on stdin to the diffctx
# binary's `hook <event>`, which prints the change's impact as
# additionalContext or nothing. The binary is the one diffctx-install.sh put
# in CLAUDE_PLUGIN_DATA at session start — one binary, one path; nothing is
# downloaded here. Every path ends in exit 0: a non-zero exit would block
# the agent's git command, and no impact review is worth a blocked commit.
set -u

[[ "${CLAUDE_PLUGIN_OPTION_IMPACT_HOOK:-true}" == "true" ]] || exit 0

# Read the event once and leave every other Bash call alone before touching
# the filesystem: the matcher is every Bash call, the interest is a few.
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

bin="${DIFFCTX_HOOK_BIN:-}"
if [[ -z "$bin" ]]; then
  root="${CLAUDE_PLUGIN_ROOT:-}"
  [[ -n "$root" && -f "$root/.claude-plugin/plugin.json" ]] || exit 0
  version=$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$root/.claude-plugin/plugin.json" | head -1)
  [[ -n "$version" ]] || exit 0
  data="${CLAUDE_PLUGIN_DATA:-${XDG_CACHE_HOME:-$HOME/.cache}/diffctx-plugin}"
  exe=""
  case "$(uname -s 2>/dev/null)" in
  MINGW* | MSYS* | CYGWIN* | Windows_NT) exe=".exe" ;;
  *) exe="" ;;
  esac
  bin="$data/bin/diffctx-$version$exe"
fi
[[ -x "$bin" ]] || exit 0

gate=()
if [[ "${CLAUDE_PLUGIN_OPTION_IMPACT_GATE:-false}" == "true" ]]; then
  gate=(--gate)
fi

# A release without the subcommand exits non-zero and prints nothing, which
# is the right answer from an older binary.
printf '%s' "$payload" | "$bin" hook "$event" "${gate[@]}" 2>/dev/null || true
exit 0
