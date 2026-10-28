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
- Hooks that hand Claude a change's impact — callers outside the diff,
  whether a test guards each, symbols several commits touched — once a
  `git commit`, `merge`, `cherry-pick`, `revert` or `push` has run (what git
  recorded, read off its reflog), before `gh pr create`, and after a
  `git diff` or `git status` Claude ran itself. Once per change (a manual
  `--mode impact` run counts; a commit of a change already shown gets a
  one-line pointer), one line when nothing outside the diff depends on it,
  never blocking. `impact_hook` turns them off; `impact_gate` (off by
  default) denies a plain `git commit` until the impact of the index it
  records was shown once, so the retry passes, and asks a commit line that
  stages for itself to stage first.

## What it runs

The MCP server starts with `uvx diffctx[mcp]==<version> mcp`,
with `UV_ISOLATED=1` so a locally installed tool cannot replace the release;
[uv](https://docs.astral.sh/uv/) must be installed. On first start uv
downloads that release and its dependencies from PyPI; after that nothing is
fetched. `uv.lock` lists the dependency set the release was tested with, each
package with its PyPI hashes; uvx resolves the dependencies itself, within
the ranges diffctx declares.

The git hooks run that same pinned package, `diffctx hook <event>`, from uv's
cache and never touch the network: a session start fills the cache in the
background. A hash of each reviewed change is
kept in your user cache so the same change is not reviewed twice; no content
is stored.

The server runs locally over stdio. It reads the git repository you point it
at (via `git diff` and `git show`), honours `.gitignore` and
`.diffctx/ignore`, and never sends repository content anywhere: no network
calls, no telemetry, no API keys, no model calls. When asked to, it can copy
its output to the local clipboard. [Privacy](https://diffctx.com/product/privacy.html).

Source, benchmarks and the paper: <https://github.com/nikolay-e/diffctx> ·
<https://diffctx.com>. Licensed under Apache-2.0.
