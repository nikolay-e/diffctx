# diffctx for Claude Code

diffctx selects the minimum code a model needs to understand a git diff.
Instead of pasting whole files, it walks the dependency graph outward from
the changed lines — callers, callees, types, tests — and stops once more
context stops paying for itself, under a hard token budget.

## What it adds

- `/diffctx:diffctx [range]` — explains a change using the fragments diffctx
  selects (default range `HEAD~1..HEAD`; `HEAD` alone means uncommitted
  changes).
- `/diffctx:impact [range]` — the blast radius of a change: impacted
  callers, tests and contracts (default `HEAD`, uncommitted changes).
- The `diffctx` MCP server with the `diffctx_context` tool, which Claude can
  call on its own during a review.

## What it runs

The MCP server starts with `uvx --from diffctx[mcp]==<version> diffctx-mcp`,
so [uv](https://docs.astral.sh/uv/) must be installed. `constraints.txt` pins
every dependency to the exact set the release was tested with. On first
start uv downloads those pinned packages from PyPI; after that nothing is
fetched.

The server runs locally over stdio. It reads the git repository you point it
at (via `git diff` and `git show`), honours `.gitignore` and
`.diffctx/ignore`, and never sends repository content anywhere: no network
calls, no telemetry, no API keys, no model calls. When asked to, it can copy
its output to the local clipboard. [Privacy](https://diffctx.com/product/privacy.html).

Source, benchmarks and the paper: <https://github.com/nikolay-e/diffctx> ·
<https://diffctx.com>. Licensed under Apache-2.0.
