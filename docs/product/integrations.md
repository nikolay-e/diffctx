# Install channels, MCP clients and the Python API

Everything the README points at in one line. Flags are in the
[command-line reference](cli.md).

## Install channels

```bash
uvx diffctx . --diff HEAD~1             # zero-install, run once via uv
pipx install diffctx                    # isolated CLI
pip install diffctx                     # into an active environment
pipx install 'diffctx[mcp]'             # + the MCP server
cargo install diffctx                   # native CLI from crates.io
npx diffctx . --diff HEAD~1             # npm wrapper over the native binary
docker run --rm -v "$PWD:/repo" ghcr.io/nikolay-e/diffctx . --diff HEAD~1
scoop bucket add diffctx https://github.com/nikolay-e/diffctx && scoop install diffctx/diffctx
```

The native binary, npm wrapper and Docker image cover diff mode with YAML/JSON
output on stdout. Tree mode, Markdown output, the `graph` subcommand and the MCP
server live in the Python package.

**Platforms.** Every [release](https://github.com/nikolay-e/diffctx/releases/latest)
carries prebuilt binaries for Linux (x86_64/aarch64), macOS (arm64/x86_64) and
Windows (x64/arm64); a release older than a target's first build lacks it
(v1.16.0 predates the macOS x86_64 and Windows arm64 archives). On Windows on
ARM, `diffctx[mcp]` builds `cryptography` from source, which needs OpenSSL.
Free-threaded CPython (`3.14t`) cannot load the `abi3` wheel, so releases after
1.16.0 also ship a `cp314t` wheel per platform.

## MCP clients

The server is published in the official MCP registry as
`io.github.nikolay-e/diffctx`.

```bash
# Claude Code plugin: the server, /diffctx:diffctx, /diffctx:impact and the commit hooks
claude plugin marketplace add nikolay-e/diffctx
claude plugin install diffctx@diffctx
# server only
claude mcp add diffctx -- uvx --from 'diffctx[mcp]' diffctx-mcp
codex mcp add diffctx -- uvx --from 'diffctx[mcp]' diffctx-mcp
gemini mcp add diffctx uvx -- --from 'diffctx[mcp]' diffctx-mcp
code --add-mcp '{"name":"diffctx","command":"uvx","args":["--from","diffctx[mcp]","diffctx-mcp"]}'
```

Every other stdio client takes the same server shape; only the file differs:

| Client | Config file | Key |
|---|---|---|
| Claude Code (project) | `.mcp.json` | `mcpServers` |
| Claude Desktop | `claude_desktop_config.json` | `mcpServers` |
| Cursor | `~/.cursor/mcp.json` | `mcpServers` |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` | `mcpServers` |
| Continue | `~/.continue/config.json` | `experimental.modelContextProtocolServers` (transport object) |
| Zed | `~/.config/zed/settings.json` | `context_servers` (`command.path`) |

```json
{ "mcpServers": { "diffctx": { "command": "uvx", "args": ["--from", "diffctx[mcp]", "diffctx-mcp"] } } }
```

With `pip install 'diffctx[mcp]'` done, `diffctx-mcp` alone replaces the `uvx`
form. Use the `diffctx-mcp` entry point, not `diffctx mcp`: the subcommand
exists only from 1.12.3 and maps a directory named `mcp` on older releases.

**The tool.** One tool, `diffctx_context`. By default (`mode=impact`) it
answers what a change reaches outside its diff — callers, the tests that reach
them, changed contracts — in under 2k tokens; `symbol` asks the same of a name
with no change. `mode=locate` ranks the code that explains a diff, and
`fragment_ids` then reads only the fragments picked; `mode=pack` returns the
code. `get_tree_map` and `get_file_context` are opt-in via
`DIFFCTX_MCP_LEGACY_TOOLS=1`; filesystem confinement via
`DIFFCTX_ALLOWED_PATHS` is in [SECURITY.md](https://github.com/nikolay-e/diffctx/blob/main/SECURITY.md).

## Python API

```python
from pathlib import Path
from diffctx import build_diff_context, map_directory, to_markdown, to_yaml

ctx = build_diff_context(
    Path("."), "HEAD~1..HEAD",
    budget_tokens=None,   # None = auto; 0 = no fragments; -1 = uncapped; N = cap on the whole artifact
    alpha=0.6, tau=0.05, full=False, scoring_mode="ego", timeout=300,
    with_raw_diff=False,  # True also embeds the raw unified diff (not charged to budget)
)
print(to_markdown(ctx))

print(to_yaml(map_directory(".", max_depth=None, no_content=False)))
```

Every JSON/YAML artifact opens with `schema: diffctx.context.v1`, validates
against [`schemas/diffctx.context.v1.json`](https://github.com/nikolay-e/diffctx/blob/main/schemas/diffctx.context.v1.json)
(generated from the engine's own type), and closes with a `provenance` block
and, when a limit cut the run short, a `coverage` block naming it.
