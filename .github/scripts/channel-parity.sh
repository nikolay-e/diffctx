#!/usr/bin/env bash
# Asks every install channel which version it serves and fetches every release
# asset the way install.js and Scoop do: unauthenticated. Writes a Markdown
# table to $REPORT and the failure count to $GITHUB_OUTPUT; exits 0 either way
# so the workflow can file the issue before failing.
set -uo pipefail

: "${VERSION:?}" "${REPO:?}" "${REPORT:?}"
GITHUB_OUTPUT=${GITHUB_OUTPUT:-/dev/null}
GITHUB_STEP_SUMMARY=${GITHUB_STEP_SUMMARY:-/dev/null}

failures=0
check() {
  local channel="$1" expected="$2" actual="$3"
  if [[ "$actual" = "$expected" ]]; then
    echo "| $channel | \`$actual\` | ok |" >>"$REPORT"
    echo "ok   $channel: $actual"
  else
    echo "| $channel | \`${actual:-<none>}\` | **expected \`$expected\`** |" >>"$REPORT"
    echo "FAIL $channel: got '${actual:-<none>}', expected '$expected'"
    failures=$((failures + 1))
  fi
  return 0
}

{
  echo "| Channel | Serves | Verdict |"
  echo "|---|---|---|"
} >"$REPORT"

# A drafted release still answers `gh release download` (authenticated) while
# the public download URL 404s — the failure that broke `npm install` twice.
draft=$(gh release view "v${VERSION}" --repo "$REPO" --json isDraft --jq '.isDraft' 2>/dev/null || echo "missing")
check "GitHub Release v${VERSION} isDraft" "false" "$draft"

assets=$(sed -nE "s/.*target: '([^']+)', archive: '([^']+)'.*/diffctx-${VERSION}-\1.\2/p" packaging/npm/install.js | sort)
for asset in $assets; do
  url="https://github.com/${REPO}/releases/download/v${VERSION}/${asset}"
  check "release asset ${asset} (unauthenticated)" "200" "$(curl --proto =https --proto-redir =https -sSIL -o /dev/null -w '%{http_code}' "$url")"
done
check "packaging/npm/checksums.json keys = install.js targets" "$(printf '%s\n' "$assets" | paste -sd' ' -)" \
  "$(jq -r 'keys[]' packaging/npm/checksums.json | sort | paste -sd' ' -)"

latest=$(curl --proto =https --proto-redir =https -fsS https://pypi.org/pypi/diffctx/json | jq -r '.info.version' || true)
check "PyPI latest" "$VERSION" "$latest"
pypi_files=$(curl --proto =https --proto-redir =https -fsS "https://pypi.org/pypi/diffctx/${VERSION}/json" | jq -r '.urls[].filename' || true)
for platform in manylinux_2_28_x86_64 manylinux_2_28_aarch64 'macosx_[0-9_]+_arm64' 'macosx_[0-9_]+_x86_64' \
  win_amd64 win_arm64; do
  for abi in cp310-abi3 cp314-cp314t; do
    have=$(printf '%s\n' "$pypi_files" | grep -cE "^diffctx-${VERSION}-${abi}-${platform}\.whl$" || true)
    check "PyPI ${abi} wheel ${platform}" "1" "$have"
  done
done

check "npm latest" "$VERSION" "$(npm view diffctx version 2>/dev/null || true)"

crate=$(curl --proto =https --proto-redir =https -fsS -H "User-Agent: diffctx-channel-parity (https://github.com/${REPO})" \
  https://crates.io/api/v1/crates/diffctx | jq -r '.crate.max_version' || true)
check "crates.io max_version" "$VERSION" "$crate"

token=$(curl --proto =https --proto-redir =https -fsS "https://ghcr.io/token?scope=repository:${REPO}:pull" | jq -r '.token' || true)
index_types='application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json'
ghcr=$(curl --proto =https --proto-redir =https -sS -o /dev/null -w '%{http_code}' -H "Authorization: Bearer ${token}" -H "Accept: ${index_types}" \
  "https://ghcr.io/v2/${REPO}/manifests/${VERSION}")
check "ghcr.io/${REPO}:${VERSION}" "200" "$ghcr"

hub=$(curl --proto =https --proto-redir =https -sS -o /dev/null -w '%{http_code}' "https://hub.docker.com/v2/repositories/nikolajer/diffctx/tags/${VERSION}")
check "docker.io/nikolajer/diffctx:${VERSION}" "200" "$hub"

registry=$(curl --proto =https --proto-redir =https -fsS "https://registry.modelcontextprotocol.io/v0.1/servers/io.github.nikolay-e%2Fdiffctx/versions/latest" |
  jq -r '.server.version' || true)
check "MCP registry latest" "$VERSION" "$registry"

check "bucket/diffctx.json (Scoop)" "$VERSION" "$(jq -r '.version' bucket/diffctx.json)"

echo "failures=$failures" >>"$GITHUB_OUTPUT"
{
  echo "## Release channel parity — expected \`${VERSION}\`"
  echo
  cat "$REPORT"
} >>"$GITHUB_STEP_SUMMARY"
echo "$failures channel(s) disagree"
