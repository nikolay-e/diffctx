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
  call on its own during a review. `mode="impact"` answers in under 2k tokens
  what a change reaches outside its own diff.
- `/diffctx:commit [message]` — the impact first, then the commit.
- Hooks that hand Claude the pending change's impact — callers outside the
  diff, whether a test guards each, symbols several commits touched — before
  `git commit`, `merge`, `cherry-pick`, `push` and `gh pr create`, and after
  a `git diff` or `git status` Claude ran itself. Once per change (a manual
  `--mode impact` run counts), silent when nothing outside the diff depends
  on it, never blocking. `impact_hook` turns them off; `impact_gate` (off by
  default) denies a commit until its impact was shown once, so the retry
  passes.

## What it runs

The MCP server starts with `uvx --from diffctx[mcp]==<version> diffctx-mcp`,
so [uv](https://docs.astral.sh/uv/) must be installed. `constraints.txt` pins
every dependency to the exact set the release was tested with. On first
start uv downloads those pinned packages from PyPI; after that nothing is
fetched.

At session start the plugin downloads the diffctx release binary for your
platform once from the GitHub release, verifies it against the checksums this
plugin carries, and keeps it in the plugin's data directory; the git hooks
only ever run that binary. A hash of each reviewed change is
kept in your user cache so the same change is not reviewed twice; no content
is stored.

The server runs locally over stdio. It reads the git repository you point it
at (via `git diff` and `git show`), honours `.gitignore` and
`.diffctx/ignore`, and never sends repository content anywhere: no network
calls, no telemetry, no API keys, no model calls. When asked to, it can copy
its output to the local clipboard. [Privacy](https://diffctx.com/product/privacy.html).

Source, benchmarks and the paper: <https://github.com/nikolay-e/diffctx> ·
<https://diffctx.com>. Licensed under Apache-2.0.
