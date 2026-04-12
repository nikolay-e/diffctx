# TreeMapper MCP Server

## Installation

```bash
pip install treemapper[mcp]
```

## Client Configuration

### Claude Code

Add to `~/.claude/mcp.json`:

```json
{
  "mcpServers": {
    "treemapper": {
      "command": "treemapper-mcp"
    }
  }
}
```

### Cursor

Add to `~/.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "treemapper": {
      "command": "treemapper-mcp"
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
          "command": "treemapper-mcp"
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
    "treemapper": {
      "command": "treemapper-mcp"
    }
  }
}
```

### Zed

Add to `~/.config/zed/settings.json`:

```json
{
  "context_servers": {
    "treemapper": {
      "command": {
        "path": "treemapper-mcp"
      }
    }
  }
}
```

## Environment Variables

- `TREEMAPPER_ALLOWED_PATHS` — colon-separated list of directories the server is
  allowed to access. When set, requests for repositories outside these paths are
  rejected.

## Available Tools

### `get_diff_context`

Returns the most relevant code fragments for understanding a git diff.

Parameters:

- `repo_path` (string, required) — absolute path to a git repository
- `diff_range` (string, default `"HEAD~1..HEAD"`) — git diff range
- `budget_tokens` (integer, default `8000`) — token budget for context selection
