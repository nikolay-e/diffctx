# diffctx — smart diff context for LLM code review

[![CI](https://github.com/nikolay-e/diffctx/actions/workflows/ci.yml/badge.svg)](https://github.com/nikolay-e/diffctx/actions/workflows/ci.yml)
[![PyPI](https://img.shields.io/pypi/v/diffctx)](https://pypi.org/project/diffctx/)
[![crates.io](https://img.shields.io/crates/v/diffctx)](https://crates.io/crates/diffctx)
[![npm](https://img.shields.io/npm/v/diffctx)](https://www.npmjs.com/package/diffctx)
[![License](https://img.shields.io/pypi/l/diffctx)](https://pypi.org/project/diffctx/)

**diffctx selects the minimum code an LLM needs to review a git diff.**
Instead of pasting whole files, it walks the dependency graph outward from the
changed lines and stops once more context stops paying for itself.

> Coming from [`treemapper`](https://pypi.org/project/treemapper/)? That name is
> deprecated. Every command, flag, and API call works unchanged:
> `treemapper` → `diffctx`, `treemapper-mcp` → `diffctx-mcp`.

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

## Install

```bash
uvx diffctx . --diff HEAD~1             # zero-install, run once via uv
pipx install diffctx                    # recommended: isolated CLI, no venv needed
pip install diffctx                     # or: into an active environment
pipx install 'diffctx[mcp]'             # + MCP server for AI assistants
```

The `[tree-sitter]` extra adds AST-level parsing for more accurate context
selection across 30+ languages.

Without Python:

```bash
cargo install diffctx                   # native CLI from crates.io
npx diffctx . --diff HEAD~1             # npm wrapper over the native binary
docker run --rm -v "$PWD:/repo" ghcr.io/nikolay-e/diffctx . --diff HEAD~1
```

The image runs as an unprivileged user and writes to stdout; to have `-o` write
into the mounted repo, add `--user "$(id -u):$(id -g)"`.

Prebuilt binaries for linux (x86_64/aarch64), macOS (arm64) and Windows (x64)
are attached to every [release](https://github.com/nikolay-e/diffctx/releases/latest).
The native binary covers diff mode with YAML/JSON output; tree mode, Markdown
output, the `graph` subcommand and the MCP server live in the Python package.
`cargo add diffctx` embeds the pipeline in a Rust project
([docs.rs](https://docs.rs/diffctx)).

## Quick start

```bash
diffctx . --diff HEAD~1       # smart context for last commit → paste into Claude/ChatGPT
diffctx . -f md -c            # full codebase export → clipboard in Markdown
```

![diffctx demo](https://raw.githubusercontent.com/nikolay-e/diffctx/main/docs/demo.gif)

*`diffctx . --diff HEAD~1` selects only the fragments an LLM needs to review the
last commit, instead of dumping every changed file in full.*

## Diff context mode

Finds the minimal set of fragments needed to understand a change — imports,
callers, type definitions, config dependencies — across 50+ file types. It
builds a code graph (imports, co-changes, type refs), propagates relevance
outward from the changed lines, and stops when relevance drops below `--tau` or
the `--budget` token cap is hit.

| Flag        | Default | Description                                                              |
|-------------|---------|--------------------------------------------------------------------------|
| `--scoring` | `ego`   | `ego` = bounded expansion around changed nodes (fast, predictable radius); `ppr` = Personalized PageRank (global, smoother decay, slower); `bm25` = lexical retrieval against the diff hunks (baseline for sparse graphs) |
| `--budget`  | auto    | Hard token cap: `N` enforces a fixed cap, `-1` disables it, `0` is a strict-zero floor (empty selection; use `--full` for changed files only) |
| `--alpha`   | 0.60    | PPR damping; higher = context clusters tighter around changes (`--scoring ppr` only) |
| `--tau`     | 0.12    | Relevance threshold for full fragment content; lower-scoring fragments are stubbed or dropped (lower = more context) |
| `--full`    | false   | Only the changed files, every fragment, no related-code context          |
| `--timeout` | 300     | Wall-clock deadline in seconds; on expiry diffctx exits 124 instead of hanging |

Calibration of `--alpha`, `--tau`, and the edge-weight priors:
[`docs/parameter-strategy.md`](docs/parameter-strategy.md). Theory:
[Context-Selection for Git Diff (Zenodo, 2026)](https://doi.org/10.5281/zenodo.18824580).

### `graph` subcommand

Explore the underlying dependency graph directly, without a diff:

```bash
diffctx graph .                                  # Mermaid graph of directory deps (default)
diffctx graph . --summary                        # cycles, hotspots, coupling metrics
diffctx graph . --level fragment -f json         # fragment-level graph as JSON
diffctx graph . --level file -f graphml -o g.xml # file-level graph as GraphML
```

## Usage

<!-- BEGIN USAGE -->
```bash
# full codebase export:
diffctx .                                # Markdown to stdout + token count
diffctx . -f md -c                       # Markdown → clipboard
diffctx . -f json -o tree.json           # JSON → file
diffctx . --no-content                   # structure only, no file contents
diffctx . --max-depth 3                  # limit depth
diffctx . -i custom.ignore               # custom ignore patterns

# diff context mode (requires git repo):
diffctx . --diff                         # uncommitted changes (working tree vs HEAD)
diffctx . --diff HEAD~1                  # context for last commit
diffctx . --diff main..feature           # context for feature branch
diffctx . --diff HEAD~1 --budget 30000   # limit to ~30k tokens
diffctx . --diff HEAD~1 -c               # diff context to clipboard
```
<!-- END USAGE -->

Every run reports token count and size on stderr — `12,847 tokens
(o200k_base), 52.3 KB` (tiktoken, the GPT-4o tokenizer; `~`-prefixed above
1 MB). `-c/--copy` copies output via `pbcopy` (macOS), `clip` (Windows), or
`wl-copy`/`xclip`/`xsel` (Linux). Unreadable files become placeholders like
`<binary file: N bytes>`, `<file too large: N bytes>`, or
`<unreadable content: not utf-8>`.

## Python API

```python
from pathlib import Path
from diffctx import build_diff_context, map_directory, to_json, to_markdown, to_text, to_yaml

ctx = build_diff_context(
    Path("."),
    "HEAD~1..HEAD",
    budget_tokens=None,       # None = auto; 0 = strict-zero floor (empty); -1 = uncapped; N = hard cap
    alpha=0.6,
    tau=0.12,
    full=False,
    scoring_mode="ego",
    timeout=300,
)
print(to_markdown(ctx))

tree = map_directory(
    ".",
    max_depth=None,
    no_content=False,
    max_file_bytes=None,
    ignore_file=None,
    no_default_ignores=False,
    whitelist_file=None,
)
print(to_yaml(tree))
```

## MCP server

diffctx includes an [MCP](https://modelcontextprotocol.io) server that lets AI
assistants (Claude Code, Cursor, Windsurf, etc.) call diff context analysis
automatically during code review. Install with `pip install 'diffctx[mcp]'`
and add it to your MCP client config (e.g. `~/.claude/mcp.json` for Claude Code):

```json
{
  "mcpServers": {
    "diffctx": {
      "command": "diffctx-mcp"
    }
  }
}
```

The server exposes a `get_diff_context` tool that assistants call when
reviewing PRs, explaining changes, or investigating broken tests. Configs for
Cursor, Continue, Windsurf, and Zed:
[`src/diffctx/mcp/README.md`](src/diffctx/mcp/README.md).

## Ignore patterns

Respects `.gitignore` and `.diffctx/ignore` automatically — hierarchically at
every directory level, with full gitignore semantics (negation `!important.log`,
anchored `/root_only.txt`). `.diffctx/whitelist` acts as an include-only filter,
and the output file is always auto-ignored. `--no-default-ignores` disables the
built-in patterns; `--no-ignores` disables all ignore rules (tree mode only).

## Token cache

Diff mode caches per-blob tokenization under
`~/Library/Caches/diffctx/token-cache` (macOS),
`$XDG_CACHE_HOME/diffctx/token-cache` (Linux) or
`%LOCALAPPDATA%\diffctx\token-cache` (Windows). It is a pure speedup: deleting
it only costs one cold run.

| Variable | Effect |
|----------|--------|
| `DIFFCTX_TOKEN_CACHE_DIR` | Relocate the cache |
| `DIFFCTX_TOKEN_CACHE_MAX_BYTES` | Size cap, default `536870912` (512 MB); `0` disables eviction |

Eviction is amortized: each run trims one of the cache's 256 shards back under
its share of the cap, oldest entries first.

## Exit codes

| Code | Meaning |
|------|---------|
| `0`  | Success — output contains content |
| `1`  | Runtime error (bad path, permission denied, etc.) |
| `2`  | Usage error (invalid flags/arguments) |
| `3`  | Environment error (`--diff` outside a git repo, git not installed, no commits yet) |
| `4`  | `--diff` produced no semantic context (clean tree, binary-only, everything filtered); output is still emitted. Deletion/rename-only diffs list `deleted_files`/`renamed_files` and exit `0` |
| `130`| Interrupted (Ctrl-C) |
| `141`| Broken pipe (e.g. piping into `head`) |

## License

Apache 2.0

---

- [Changelog](CHANGELOG.md)
- [Security policy](SECURITY.md) — threat model and vulnerability reporting
- [Parameter strategy](docs/parameter-strategy.md) — how `--alpha`,
  `--tau`, and edge weights are calibrated
