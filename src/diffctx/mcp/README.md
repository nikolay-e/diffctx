# diffctx MCP Server

## Installation

```bash
pip install diffctx[mcp]
```

The server starts as either `diffctx-mcp` or `diffctx mcp` — the two are
equivalent. Zero-install: `uvx --from 'diffctx[mcp]' diffctx mcp`.

## Client Configuration

### Claude Code

```bash
claude mcp add diffctx -- diffctx-mcp
```

Or add to your project's `.mcp.json`:

```json
{
  "mcpServers": {
    "diffctx": {
      "command": "diffctx-mcp"
    }
  }
}
```

### Cursor

Add to `~/.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "diffctx": {
      "command": "diffctx-mcp"
    }
  }
}
```

### Continue

Add to `~/.continue/config.json`:

```json
{
  "experimental": {
    "modelContextProtocolServers": [
      {
        "transport": {
          "type": "stdio",
          "command": "diffctx-mcp"
        }
      }
    ]
  }
}
```

### Windsurf

Add to `~/.codeium/windsurf/mcp_config.json`:

```json
{
  "mcpServers": {
    "diffctx": {
      "command": "diffctx-mcp"
    }
  }
}
```

### Zed

Add to `~/.config/zed/settings.json`:

```json
{
  "context_servers": {
    "diffctx": {
      "command": {
        "path": "diffctx-mcp"
      }
    }
  }
}
```

## Environment Variables

- `DIFFCTX_ALLOWED_PATHS` — OS-pathsep-separated list (`:` on POSIX, `;` on
  Windows) of directories the server is allowed to access. When set, requests
  for repositories outside these paths are rejected.

## Available Tools

### `get_diff_context`

Returns the most relevant code fragments for understanding a git diff.

Parameters:

- `repo_path` (string, required) — absolute path to a git repository
- `diff_range` (string, default `"HEAD~1..HEAD"`) — git diff range
- `budget_tokens` (integer, default `8000`) — token budget for selection
- `clipboard` (boolean, default `false`) — copy the result to the clipboard
  and return a short confirmation instead of the full context

### `get_tree_map`

Returns a structured map of a codebase — directory tree with file contents in
YAML or Markdown, respecting `.gitignore` and skipping binaries/build
artifacts. Requires a git repository.

Parameters:

- `repo_path` (string, required) — absolute path to a git repository
- `subdirectory` (string, default `""`) — optional path under `repo_path` to
  map instead of the whole repository
- `output_format` (string, default `"yaml"`) — `"yaml"` or `"md"`
- `no_content` (boolean, default `false`) — emit structure only, skip contents
- `max_depth` (integer, optional) — limit traversal depth
- `max_file_bytes` (integer, default `262144` = 256 KB) — per-file content cap
- `clipboard` (boolean, default `false`) — copy to clipboard instead of
  returning the map
- `max_tokens` (integer, default `25000`) — ceiling on the returned output; if
  the map exceeds it, an advisory message is returned instead of the content
  (narrow with `subdirectory`/`no_content`/`max_depth`, or raise the ceiling)

### `get_file_context`

Reads files by glob pattern, formatted for LLM consumption. Works on any
directory (no git required).

Parameters:

- `repo_path` (string, required) — absolute path to a directory
- `patterns` (list of strings, required) — glob patterns, e.g.
  `["src/**/*.py"]`
- `max_files` (integer, default `50`) — maximum number of files to include
- `max_file_bytes` (integer, default `262144` = 256 KB) — per-file content cap; larger
  files are listed but their content is skipped
- `clipboard` (boolean, default `false`) — copy to clipboard instead of
  returning the content
- `dry_run` (boolean, default `false`) — preview which files match without
  reading their content
- `max_tokens` (integer, default `25000`) — ceiling on the returned output; if
  the content exceeds it, an advisory message is returned instead (tighten
  `patterns`, lower `max_files`, use `dry_run=true`, or raise the ceiling)
