# diffctx for Antigravity, Codex and Gemini CLI

diffctx selects the minimum code a model needs to understand a git diff.
Instead of pasting whole files, it walks the dependency graph outward from
the changed lines — callers, callees, types, tests — and stops once more
context stops paying for itself, under a hard token budget.

## Install

```bash
# Antigravity (App, CLI, IDE)
agy plugin install https://github.com/nikolay-e/diffctx/tree/main/plugins/diffctx

# Codex
codex plugin marketplace add nikolay-e/diffctx
codex plugin add diffctx@diffctx

# Gemini CLI (from the release after 1.20.0; before it: gemini extensions install ./plugins/diffctx)
gemini extensions install https://github.com/nikolay-e/diffctx
```

Claude Code has its own plugin: `claude plugin marketplace add nikolay-e/diffctx`
and `claude plugin install diffctx@diffctx`.

## What it bundles

- The `diffctx` MCP server exposing `diffctx_context` (`impact`, `pack`,
  `locate` and `symbol` modes).
- `/diffctx` — explains a change from its impact and graph-ranked surrounding
  fragments.
- `/diffctx-impact` — blast radius of uncommitted work or a commit range:
  callers outside the diff, guarding tests and crossed contracts.
- `/diffctx-commit` — checks impact before committing and accounts for
  unguarded callers.
- Standing rule (`rules/diffctx.md`, Gemini CLI's context file) for structural
  symbol lookup and pre-commit impact checks.
- Lifecycle hooks, one bridge (`scripts/diffctx-hook.py <host>`) per host's
  hook file: Antigravity `hooks.json`, Codex `com.openai/hooks/hooks.json`,
  Gemini CLI `hooks/hooks.json`. They warm uv's cache at session start and
  hand the agent the impact after `git commit`, `merge`, `cherry-pick`,
  `revert`, `push`, `git diff`, `git status` and symbol searches
  (`DIFFCTX_IMPACT_HOOK=false` disables them; `DIFFCTX_IMPACT_GATE=true`
  enables the strict pre-commit gate). Codex runs a plugin's hooks only once
  you trust them in `/hooks`, and Codex CLI 0.172 `exec` runs none; the MCP
  server and skills work everywhere.

## What it runs

The MCP server and hooks run `uvx diffctx[mcp]==<version>` with
`UV_ISOLATED=1` so a locally installed tool cannot replace the release;
[uv](https://docs.astral.sh/uv/) and Python 3 must be installed. Everything
runs locally over stdio: no network calls beyond uv's initial PyPI fetch, no
telemetry, no API keys, no model calls.
[Privacy](https://diffctx.com/product/privacy.html).

Source, benchmarks and the paper: <https://github.com/nikolay-e/diffctx> ·
<https://diffctx.com>. Licensed under the [PolyForm Shield License 1.0.0][license].

[license]: https://github.com/nikolay-e/diffctx/blob/main/LICENSE.md
