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

Since 1.13 the default surface is **one tool**. Tool definitions are sent on
every request of every session, before any work happens, so the three-tool
surface cost 1063 tokens of every window it was installed in; the single tool
costs 267 (-75%). Measured with `o200k_base` over each definition's serialized
`{name, description, inputSchema}`.

### `diffctx_context`

Two-call flow: rank first, then read only what you chose.

1. `mode="locate"` (**default**) returns compact `diffctx.locate.v1` JSON — the
   ranked selection as a navigation list with line spans, scores and provenance
   reasons, and no source bodies. Cost scales with the number of ranked items,
   not with their source: a few hundred tokens for a small change, ~1.9k for a
   38-item ranking on a large one — against the thousands a pack of the same
   selection costs.

   The ranking also discloses its own gaps when it has any: a `coverage` block
   (changed files the parser could not see inside, changed files with no graph
   edge, a truncated diffusion, and a heuristic `confidence`) and an `overflow`
   ranking of candidates the budget left behind, with reasons and no bodies.
   Both are omitted when there is nothing to report, so a clean run pays
   nothing for them. Treat `confidence` as what it is documented to be — how
   much of the changed surface the run could see and fit, not a claim that the
   selection is right.
2. Pass ids built from that ranking's own fields — `"<path>:<lines>"`, e.g.
   `"src/calc.py:12-40"` — back as `fragment_ids` to get their source.

`mode="pack"` skips the split and returns the full selected context in one
call, which is what the pre-1.13 default did.

- `repo_path` (string, required) — absolute path to a git repository
- `diff_ref` (string, default `"HEAD~1..HEAD"`)
- `mode` (`"locate"` default, or `"pack"`)
- `budget_tokens` (integer, default `8000`) — selection budget; `-1`
  unlimited (still capped by `max_tokens`), `0` strict-zero floor
- `fragment_ids` (list of strings, optional) — read these instead of ranking.
  Takes precedence over `mode`. Max 40 per call; bodies are read at the diff
  range's end revision, so line spans from a historical range resolve against
  that revision rather than the working tree. `.gitignore` and
  `.diffctx/ignore` are enforced — an id cannot read what the repo withholds
  from the other modes.
- `include_raw_diff` (boolean, default `false`) — embed git's raw unified
  diff ahead of the fragments (additive, not charged to the budget);
  `mode="pack"` only
- `clipboard` (boolean, default `false`) — copy instead of returning
- `max_tokens` (integer, default `25000`)

### Legacy tools (opt-in)

`get_tree_map` and `get_file_context` are off by default since 1.13: they are
strictly wider than a diff question needs, their definitions cost every session
that never calls them, and MCP hosts already ship their own file-reading tools.
Set `DIFFCTX_MCP_LEGACY_TOOLS=1` in the server's environment to restore the
pre-1.13 surface.

```json
{
  "mcpServers": {
    "diffctx": {
      "command": "diffctx-mcp",
      "env": { "DIFFCTX_MCP_LEGACY_TOOLS": "1" }
    }
  }
}
```

#### `get_tree_map`

- `repo_path` (string, required); `subdirectory` (string, default `""`)
- `output_format` (`"yaml"` default, or `"md"`); `no_content` (boolean)
- `max_depth` (integer, optional); `max_file_bytes` (default `262144`)
- `clipboard`, `max_tokens` as above

#### `get_file_context`

Works on any directory, no git required.

- `repo_path` (string, required); `patterns` (list of globs, required)
- `max_files` (default `50`); `max_file_bytes` (default `262144`)
- `dry_run` (boolean, default `false`) — preview matches without reading
- `clipboard`, `max_tokens` as above
