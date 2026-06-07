# diffctx — smart diff context for LLM code review

[![CI](https://github.com/nikolay-e/diffctx/actions/workflows/ci.yml/badge.svg)](https://github.com/nikolay-e/diffctx/actions/workflows/ci.yml)
[![PyPI](https://img.shields.io/pypi/v/diffctx)](https://pypi.org/project/diffctx/)
[![License](https://img.shields.io/pypi/l/diffctx)](https://pypi.org/project/diffctx/)

**diffctx selects the minimum code an LLM needs to review a git diff.**
Instead of pasting whole files, it walks the dependency graph from the changed
lines outward and stops as soon as additional context stops paying for itself.

## Why not just use `tree` or repomix?

| | `tree` | repomix | Claude Code Review | **diffctx** |
|---|:---:|:---:|:---:|:---:|
| **Primary use case** | directory listing | full repo export | automated PR review | **diff context for code review** |
| Smart diff context | ✗ | ✗ | ✓ | ✓ |
| Works with any LLM | ✓ | ✓ | Claude only | ✓ |
| Free / local / offline | ✓ | ✓ | $15–25/review | ✓ |
| GitHub required | ✗ | ✗ | ✓ | ✗ |
| Multiple output formats | ✗ | limited | — | YAML/JSON/MD/txt |
| Python API | ✗ | ✗ | ✗ | ✓ |
| MCP server | ✗ | ✗ | ✗ | ✓ |

## Install (30 seconds)

```bash
pipx install diffctx                    # recommended — isolated CLI, no venv needed
pip install diffctx                     # or: into an active environment
pipx install 'diffctx[tree-sitter]'     # + AST parsing for smarter diff context
pipx install 'diffctx[mcp]'             # + MCP server for AI assistants
```

> For everyday use, install once with `pipx` and call `diffctx` from any
> directory. Do **not** `source` the project's `.venv` to run `diffctx` from
> another repo — that runs a working-tree build and mutates the shell's
> `PATH`/`PYTHONHOME` for every subsequent command.

```bash
diffctx . --diff HEAD~1       # smart context for last commit → paste into Claude/ChatGPT
diffctx . -f md -c            # full export → clipboard in Markdown
```

![diffctx demo: running `diffctx . --diff HEAD~1` inside a git repo and copying the relevance-ranked YAML output to the clipboard for an LLM](https://raw.githubusercontent.com/nikolay-e/diffctx/main/docs/demo.gif)

*Demo: `diffctx . --diff HEAD~1` selects only the fragments — functions,
imports, type definitions — that an LLM actually needs to review the last
commit, instead of dumping every changed file in full.*

**Standalone binary** (no Python required): download from the
[releases page](https://github.com/nikolay-e/diffctx/releases/latest).

> Diff context mode works out of the box. Adding `[tree-sitter]` enables AST-level
> parsing for more accurate context selection across 12 languages.

## Diff Context Mode

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

Builds a code graph (imports, co-changes, type refs) and propagates
relevance from changed lines outward across it. Three scoring modes are
available — pick one with `--scoring`:

| `--scoring` | What it does                                              |
|-------------|-----------------------------------------------------------|
| `ego` (default) | Bounded ego-network expansion around changed nodes — fast, predictable radius, the current default |
| `ppr`       | Personalized PageRank with damping `--alpha` — global, smoother decay, slower |
| `bm25`      | Lexical fragment retrieval against the diff hunks — useful as a baseline / fallback when the graph is sparse |

Selection stops when relevance drops below `--tau` (the minimum score a
fragment must beat to be kept), or once `--budget` tokens have been
emitted, whichever comes first.

| Flag        | Default | Description                                                              |
|-------------|---------|--------------------------------------------------------------------------|
| `--scoring` | `ego`   | Scoring mode: `ego`, `ppr`, or `bm25`                                    |
| `--budget`  | auto    | Token cap. `auto` lets selection converge; `-1` disables the cap; `N` enforces a fixed cap |
| `--alpha`   | 0.60    | How tightly context clusters around changes (PPR damping; 0–1, higher = more focused) |
| `--tau`     | 0.08    | Minimum relevance required to include a fragment (lower = more context)  |
| `--full`    | false   | Include every changed fragment; skip the smart-selection step entirely   |

Calibration of `--alpha`, `--tau`, and the edge-weight priors is documented
in [`docs/parameter-strategy.md`](docs/parameter-strategy.md).

*Theory: [Context-Selection for Git Diff (Zenodo, 2026)](https://doi.org/10.5281/zenodo.18824580).*

### `graph` subcommand

For exploring the underlying dependency graph directly (without a diff),
use the `graph` subcommand:

```bash
diffctx graph .                                  # Mermaid graph of directory deps (default)
diffctx graph . --summary                        # cycles, hotspots, coupling metrics
diffctx graph . --level fragment -f json         # fragment-level graph as JSON
diffctx graph . --level file -f graphml -o g.xml # file-level graph as GraphML
```

| Flag        | Default      | Description                                              |
|-------------|--------------|----------------------------------------------------------|
| `-f/--format` | `mermaid`  | Output format: `mermaid`, `json`, or `graphml`           |
| `--level`   | `directory`  | Granularity: `fragment`, `file`, or `directory`          |
| `--summary` | false        | Print graph statistics (cycles, hotspots, coupling)      |

## Usage

<!-- BEGIN USAGE -->
```bash
# full codebase export:
diffctx .                                # YAML to stdout + token count
diffctx . -f md -c                       # Markdown → clipboard
diffctx . -f json -o tree.json           # JSON → file
diffctx . --no-content                   # structure only, no file contents
diffctx . --max-depth 3                  # limit depth
diffctx . -i custom.ignore               # custom ignore patterns

# diff context mode (requires git repo):
diffctx . --diff HEAD~1                  # context for last commit
diffctx . --diff main..feature           # context for feature branch
diffctx . --diff HEAD~1 --budget 30000   # limit to ~30k tokens
diffctx . --diff HEAD~1 -c               # diff context to clipboard
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
diffctx . -c                       # copy (stdout suppressed, stderr: token count)
diffctx . -c -o tree.yaml          # copy + save to file
```

**System Requirements:**

- **macOS:** `pbcopy` (pre-installed)
- **Windows:** `clip` (pre-installed)
- **Linux (Wayland):** `wl-copy`
- **Linux (X11):** `xclip` or `xsel`

## Python API

```python
from diffctx import map_directory
from diffctx import to_yaml, to_json, to_text, to_markdown

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
from diffctx import build_diff_context, to_yaml

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

diffctx includes an [MCP](https://modelcontextprotocol.io) server that lets
AI assistants (Claude Code, Cursor, Windsurf, etc.) call diff context analysis
automatically during code review.

```bash
pip install 'diffctx[mcp]'
```

Add to your MCP client config (e.g. `~/.claude/mcp.json` for Claude Code):

```json
{
  "mcpServers": {
    "diffctx": {
      "command": "diffctx-mcp"
    }
  }
}
```

The server exposes a `get_diff_context` tool. Your AI assistant will
automatically call it when reviewing PRs, explaining changes, or investigating
broken tests — no manual invocation needed.

See [`src/diffctx/mcp/README.md`](src/diffctx/mcp/README.md) for configs
for Cursor, Continue, Windsurf, and Zed.

## Ignore Patterns

Respects `.gitignore` and `.diffctx/ignore` automatically.
Use `--no-default-ignores` to disable built-in patterns
(`.gitignore` and `.diffctx/ignore` still apply).

- Hierarchical: nested ignore files at each directory level
- Negation patterns: `!important.log` un-ignores a file
- Anchored patterns: `/root_only.txt` matches only in root
- Output file is always auto-ignored

Auto-discovered files:

- `.diffctx/ignore` — diffctx-specific ignore patterns
- `.diffctx/whitelist` — Include-only filter (only matched files included)

## Content Placeholders

- `<file too large: N bytes>` — exceeds `--max-file-bytes`
- `<binary file: N bytes>` — binary file detected
- `<unreadable content: not utf-8>` — not valid UTF-8
- `<unreadable content>` — permission denied or I/O error

## License

Apache 2.0

---

- [Changelog](CHANGELOG.md)
- [Security policy](SECURITY.md) — threat model and vulnerability reporting
- [Parameter strategy](docs/parameter-strategy.md) — how `--alpha`,
  `--tau`, and edge weights are calibrated
