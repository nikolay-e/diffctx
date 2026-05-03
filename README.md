# TreeMapper

[![CI](https://github.com/nikolay-e/treemapper/actions/workflows/ci.yml/badge.svg)](https://github.com/nikolay-e/treemapper/actions/workflows/ci.yml)
[![PyPI](https://img.shields.io/pypi/v/treemapper)](https://pypi.org/project/treemapper/)
[![Downloads](https://img.shields.io/pypi/dm/treemapper)](https://pypi.org/project/treemapper/)
[![License](https://img.shields.io/pypi/l/treemapper)](https://pypi.org/project/treemapper/)

**Smart diff context for LLM code review.** Selects the minimal set of code
fragments needed to understand a git change — instead of dumping entire files.

Also exports full codebase structure + contents in YAML/JSON/MD/txt.
Works with any LLM. Available as CLI, Python API, and MCP server.
100% local, free, no GitHub dependency.

```bash
pipx install treemapper

treemapper . --diff HEAD~1       # smart context for last commit → paste into Claude/ChatGPT
treemapper . -f md -c           # full export → clipboard in Markdown
```

![demo](docs/demo.gif)

## Why not just use `tree` or repomix?

| | `tree` | repomix | Claude Code Review | **TreeMapper** |
|---|:---:|:---:|:---:|:---:|
| **Primary use case** | directory listing | full repo export | automated PR review | **diff context for code review** |
| Smart diff context | ✗ | ✗ | ✓ | ✓ |
| Works with any LLM | ✓ | ✓ | Claude only | ✓ |
| Free / local / offline | ✓ | ✓ | $15–25/review | ✓ |
| GitHub required | ✗ | ✗ | ✓ | ✗ |
| Multiple output formats | ✗ | limited | — | YAML/JSON/MD/txt |
| Python API | ✗ | ✗ | ✗ | ✓ |
| MCP server | ✗ | ✗ | ✗ | ✓ |

## Installation

```bash
pipx install treemapper                    # recommended: isolated, no venv needed
pip install treemapper                     # or with pip
pip install 'treemapper[tree-sitter]'      # + AST parsing for smarter diff context
pip install 'treemapper[mcp]'             # + MCP server for AI assistants
```

**Standalone binary** (no Python required): download from the
[releases page](https://github.com/nikolay-e/treemapper/releases/latest).

> Diff context mode works out of the box. Adding `[tree-sitter]` enables AST-level
> parsing for more accurate context selection across 12 languages.

## Diff Context Mode

**Paper:** [Context-Selection for Git Diff (Zenodo, 2026)](https://doi.org/10.5281/zenodo.18824580)

Automatically finds the minimal set of code fragments needed to understand
a change — imports, callers, type definitions, config dependencies — without
dumping entire files. Understands 50+ file types.

```yaml
name: myproject
type: diff_context
fragment_count: 5
fragments:
  - path: src/main.py
    lines: "10-25"
    kind: function
    symbol: process_data
    content: |
      def process_data(items):
          ...
```

### How it works

Uses Personalized PageRank on a code graph (imports, co-changes, type refs)
to propagate relevance from changed lines outward. Stops when signal decays
below threshold τ, or at an explicit `--budget` token limit.

| Flag       | Default | Description                              |
|------------|---------|------------------------------------------|
| `--budget` | none    | Token limit (convergence-based by default) |
| `--full`   | false   | Include all changed code, skip selection |
| `--alpha`  | 0.60    | PPR damping factor                       |
| `--tau`    | 0.08    | Convergence threshold                    |

## Usage

<!-- BEGIN USAGE -->
```bash
# full codebase export:
treemapper .                                # YAML to stdout + token count
treemapper . -f md -c                       # Markdown → clipboard
treemapper . -f json -o tree.json           # JSON → file
treemapper . --no-content                   # structure only, no file contents
treemapper . --max-depth 3                  # limit depth
treemapper . -i custom.ignore               # custom ignore patterns

# diff context mode (requires git repo):
treemapper . --diff HEAD~1                  # context for last commit
treemapper . --diff main..feature           # context for feature branch
treemapper . --diff HEAD~1 --budget 30000   # limit to ~30k tokens
treemapper . --diff HEAD~1 -c               # diff context to clipboard
```
<!-- END USAGE -->

Full codebase export output format:

```yaml
name: myproject
type: directory
children:
  - name: main.py
    type: file
    content: |
      def hello():
          print("Hello, World!")
  - name: utils/
    type: directory
    children:
      - name: helpers.py
        type: file
        content: |
          def add(a, b):
              return a + b
```

## Token Counting

Token count and size are always displayed on stderr:

```text
12,847 tokens (o200k_base), 52.3 KB
```

For large outputs (>1MB), approximate counts with `~` prefix:

```text
~125,000 tokens (o200k_base), 5.2 MB
```

Uses tiktoken with `o200k_base` encoding (GPT-4o tokenizer).

## Clipboard Support

Copy output directly to clipboard with `-c` or `--copy`:

```bash
treemapper . -c                       # copy (stdout suppressed, stderr: token count)
treemapper . -c -o tree.yaml          # copy + save to file
```

**System Requirements:**

- **macOS:** `pbcopy` (pre-installed)
- **Windows:** `clip` (pre-installed)
- **Linux (Wayland):** `wl-copy`
- **Linux (X11):** `xclip` or `xsel`

## Python API

```python
from treemapper import map_directory
from treemapper import to_yaml, to_json, to_text, to_markdown

tree = map_directory(
    path,                     # directory path
    max_depth=None,           # limit traversal depth
    no_content=False,         # exclude file contents
    max_file_bytes=None,      # skip large files
    ignore_file=None,         # custom ignore file
    no_default_ignores=False, # disable default ignores
    whitelist_file=None,      # include-only filter
)

yaml_str = to_yaml(tree)
json_str = to_json(tree)
text_str = to_text(tree)
md_str = to_markdown(tree)

# Diff context mode
from pathlib import Path
from treemapper import build_diff_context, to_yaml

ctx = build_diff_context(
    Path("."),                # repository root
    "HEAD~1..HEAD",           # diff range; also accepts "main..feature"
    budget_tokens=None,       # None = convergence-based (default)
                              #   0  = diff only, no expansion (recall floor)
                              #  <0  = unlimited (10M-token soft ceiling)
                              #  >0  = explicit token cap
    alpha=0.6,                # PPR damping factor
    tau=0.08,                 # stopping threshold
    full=False,               # skip smart selection
)
yaml_str = to_yaml(ctx)
```

## MCP Server

TreeMapper includes an [MCP](https://modelcontextprotocol.io) server that lets
AI assistants (Claude Code, Cursor, Windsurf, etc.) call diff context analysis
automatically during code review.

```bash
pip install 'treemapper[mcp]'
```

Add to your MCP client config (e.g. `~/.claude/mcp.json` for Claude Code):

```json
{
  "mcpServers": {
    "treemapper": {
      "command": "treemapper-mcp"
    }
  }
}
```

The server exposes a `get_diff_context` tool. Your AI assistant will
automatically call it when reviewing PRs, explaining changes, or investigating
broken tests — no manual invocation needed.

See [`src/treemapper/mcp/README.md`](src/treemapper/mcp/README.md) for configs
for Cursor, Continue, Windsurf, and Zed.

## Ignore Patterns

Respects `.gitignore` and `.treemapper/ignore` automatically.
Use `--no-default-ignores` to disable built-in patterns
(`.gitignore` and `.treemapper/ignore` still apply).

- Hierarchical: nested ignore files at each directory level
- Negation patterns: `!important.log` un-ignores a file
- Anchored patterns: `/root_only.txt` matches only in root
- Output file is always auto-ignored

Auto-discovered files:

- `.treemapper/ignore` — TreeMapper-specific ignore patterns
- `.treemapper/whitelist` — Include-only filter (only matched files included)

## Content Placeholders

- `<file too large: N bytes>` — exceeds `--max-file-bytes`
- `<binary file: N bytes>` — binary file detected
- `<unreadable content: not utf-8>` — not valid UTF-8
- `<unreadable content>` — permission denied or I/O error

## License

Apache 2.0
