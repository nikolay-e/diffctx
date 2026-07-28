# diffctx MCP Server

One-line setup for Claude Code / Codex CLI / Gemini CLI / VS Code lives in
the [main README](../../../README.md#mcp-server). Zero-install command:
`uvx --from 'diffctx[mcp]' diffctx-mcp` — use the `diffctx-mcp` entry point,
not the `diffctx mcp` subcommand, which only exists from 1.12.3 onward and
would map a directory named `mcp` on older releases.

## JSON client configs

All stdio clients take the same server shape; only the config file differs:

| Client | Config file | Key |
|---|---|---|
| Claude Code (project) | `.mcp.json` | `mcpServers` |
| Claude Desktop | `claude_desktop_config.json` | `mcpServers` |
| Cursor | `~/.cursor/mcp.json` | `mcpServers` |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` | `mcpServers` |
| Continue | `~/.continue/config.json` | `experimental.modelContextProtocolServers` (transport object) |
| Zed | `~/.config/zed/settings.json` | `context_servers` (`command.path`) |

```json
{
  "mcpServers": {
    "diffctx": {
      "command": "uvx",
      "args": ["--from", "diffctx[mcp]", "diffctx-mcp"]
    }
  }
}
```

With `pip install 'diffctx[mcp]'` already done, `"command": "diffctx-mcp"`
with no args works everywhere instead.

Filesystem confinement via `DIFFCTX_ALLOWED_PATHS`: see
[SECURITY.md](../../../SECURITY.md).

## Tools

The server pins `tau=0.12` (the CLI default) and applies a 300 s wall-clock
deadline per tool call; every tool caps its response at `max_tokens`
(default `25000`) and returns an advisory message instead of oversized
content.

### `get_diff_context`

- `repo_path` (string, required) — absolute path to a git repository
- `diff_range` (string, default `"HEAD~1..HEAD"`)
- `budget_tokens` (integer, default `8000`) — selection budget; `-1`
  unlimited (still capped by `max_tokens`), `0` strict-zero floor
- `include_raw_diff` (boolean, default `false`) — embed git's raw unified
  diff ahead of the fragments (additive, not charged to the budget)
- `clipboard` (boolean, default `false`) — copy instead of returning
- `max_tokens` (integer, default `25000`)

### `get_tree_map`

- `repo_path` (string, required); `subdirectory` (string, default `""`)
- `output_format` (`"yaml"` default, or `"md"`); `no_content` (boolean)
- `max_depth` (integer, optional); `max_file_bytes` (default `262144`)
- `clipboard`, `max_tokens` as above

### `get_file_context`

Works on any directory, no git required.

- `repo_path` (string, required); `patterns` (list of globs, required)
- `max_files` (default `50`); `max_file_bytes` (default `262144`)
- `dry_run` (boolean, default `false`) — preview matches without reading
- `clipboard`, `max_tokens` as above
