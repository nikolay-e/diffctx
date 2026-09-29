#!/usr/bin/env bash
# SessionStart: put the diffctx release binary for this platform where the
# PreToolUse hook expects it, once per plugin version. The download never
# happens on the commit path: this runs when a session starts, bounded to a
# minute so a slow network delays the session by that much at most, and a
# failed attempt is remembered so an offline machine pays for it once every
# few hours, not on every session. Every path ends in exit 0.
set -u

[[ "${CLAUDE_PLUGIN_OPTION_IMPACT_HOOK:-true}" == "true" ]] || exit 0
root="${CLAUDE_PLUGIN_ROOT:-}"
[[ -n "$root" && -f "$root/.claude-plugin/plugin.json" ]] || exit 0
version=$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$root/.claude-plugin/plugin.json" | head -1)
[[ -n "$version" ]] || exit 0

data="${CLAUDE_PLUGIN_DATA:-${XDG_CACHE_HOME:-$HOME/.cache}/diffctx-plugin}"
bindir="$data/bin"
exe=""
case "$(uname -s 2>/dev/null)" in
MINGW* | MSYS* | CYGWIN* | Windows_NT) exe=".exe" ;;
*) exe="" ;;
esac
bin="$bindir/diffctx-$version$exe"

# The session's one line of standing context, as a command to run rather
# than a tool to consider: models follow a concrete recipe more readily.
# A release that predates the hooks takes no recipe: telling the session
# to run a mode the binary lacks would hand it an error.
recipe() {
  local binary="$1"
  "$binary" hook pretooluse --help >/dev/null 2>&1 || return 0
  printf '%s' "{\"hookSpecificOutput\":{\"hookEventName\":\"SessionStart\",\"additionalContext\":\"diffctx is installed. Before committing or pushing a multi-file change, run: $binary . --diff --mode impact -f md  (what the change reaches outside its diff: callers, their tests, cross-commit overlap). The plugin also injects this before git commit/merge/push and after git diff.\"}}"
  echo
  return 0
}

if [[ -x "$bin" ]]; then
  recipe "$bin"
  exit 0
fi

# Back off after a failure: six hours between attempts.
failed="$data/install-failed-$version"
if [[ -f "$failed" && -n "$(find "$failed" -mmin -360 2>/dev/null)" ]]; then
  exit 0
fi

target_for_host() {
  local os arch
  os=$(uname -s 2>/dev/null)
  arch=$(uname -m 2>/dev/null)
  case "$arch" in
  x86_64 | amd64) arch=x86_64 ;;
  arm64 | aarch64) arch=aarch64 ;;
  *) return 1 ;;
  esac
  case "$os" in
  Darwin) echo "$arch-apple-darwin tar.gz" ;;
  Linux) echo "$arch-unknown-linux-gnu tar.gz" ;;
  MINGW* | MSYS* | CYGWIN* | Windows_NT) echo "$arch-pc-windows-msvc zip" ;;
  *) return 1 ;;
  esac
  return 0
}

sha256_of() {
  local file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{print $1}'
  else
    return 1
  fi
  return 0
}

tmp=""
# shellcheck disable=SC2329  # invoked by the trap
cleanup() {
  [[ -n "$tmp" ]] && rm -rf "$tmp"
  return 0
}
trap cleanup EXIT

fail() {
  mkdir -p "$data" 2>/dev/null && : >"$failed"
  exit 0
}

read -r target kind < <(target_for_host) || fail
asset="diffctx-$version-$target.$kind"
# A version whose checksum this plugin does not carry is not installed: the
# plugin's own checksums.json is the trust anchor, not the download. It is
# also not a failure: a release bumps the manifest before its binaries and
# checksums exist, and a session in that window must try again next time,
# not sit out the six-hour backoff.
expected=$(sed -n "s/.*\"$asset\"[[:space:]]*:[[:space:]]*\"\([0-9a-f]\{64\}\)\".*/\1/p" "$root/checksums.json" 2>/dev/null | head -1)
[[ -n "$expected" ]] || exit 0
command -v curl >/dev/null 2>&1 || fail
tmp=$(mktemp -d 2>/dev/null) || fail
curl --proto '=https' --tlsv1.2 -fsSL --max-time 60 \
  -o "$tmp/$asset" "https://github.com/nikolay-e/diffctx/releases/download/v$version/$asset" || fail
actual=$(sha256_of "$tmp/$asset") || fail
[[ "$actual" == "$expected" ]] || fail
mkdir -p "$tmp/out" "$bindir" || fail
if [[ "$kind" == "zip" ]]; then
  command -v unzip >/dev/null 2>&1 || fail
  unzip -q -o "$tmp/$asset" -d "$tmp/out" || fail
else
  tar -xzf "$tmp/$asset" -C "$tmp/out" || fail
fi
if ! mv "$tmp/out/diffctx$exe" "$bin"; then fail; fi
if ! chmod +x "$bin"; then fail; fi
rm -f "$failed"
recipe "$bin"
exit 0
