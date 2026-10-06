# diffctx — what a git change reaches, and the code to review it

[![CI](https://github.com/nikolay-e/diffctx/actions/workflows/ci.yml/badge.svg)](https://github.com/nikolay-e/diffctx/actions/workflows/ci.yml)
[![PyPI](https://img.shields.io/pypi/v/diffctx)](https://pypi.org/project/diffctx/)
[![crates.io](https://img.shields.io/crates/v/diffctx)](https://crates.io/crates/diffctx)
[![npm](https://img.shields.io/npm/v/diffctx)](https://www.npmjs.com/package/diffctx)
[![MCP Registry](https://img.shields.io/badge/MCP_Registry-io.github.nikolay--e%2Fdiffctx-blue)](https://registry.modelcontextprotocol.io/v0/servers?search=io.github.nikolay-e/diffctx)

**diffctx tells an agent or a reviewer what a change reaches outside its
diff** — the callers, the tests that reach them, the contracts it crosses —
**and selects the code needed to understand it** under a token budget. Local,
deterministic, no index, no model calls. Caller resolution is static and per
language; where it cannot resolve, the answer says so instead of printing a
zero.

> Formerly published as `treemapper` — every command, flag, and API call works unchanged.

## Install

```bash
# Claude Code: the MCP server, /diffctx:impact, and hooks that run it before commit and push
claude plugin marketplace add nikolay-e/diffctx
claude plugin install diffctx@diffctx

# any MCP client
claude mcp add diffctx -- uvx --from 'diffctx[mcp]' diffctx-mcp

# CLI, zero-install
uvx diffctx . --diff HEAD~1
```

pipx, pip, cargo, npm, Docker, Scoop, other MCP clients and the Python API:
[integrations](docs/product/integrations.md).

## Usage

```bash
diffctx . --diff --mode impact          # uncommitted work: callers outside the diff, their tests, contracts
diffctx . --symbol parse_config         # the same for a name, no change needed
diffctx . --diff main...feature         # the code to review a branch, packed under the auto budget
diffctx . --diff HEAD~1 --budget 12000  # the last commit, capped at 12k o200k tokens
diffctx . --diff 24h --mode locate      # today's work as ranked JSON, no source bodies
diffctx .                               # whole-tree export, Markdown
```

`--diff` takes a git range, `staged`, or a duration window ending now (`24h`,
`90min`, `2w`). Every flag, default and exit code:
[command-line reference](docs/product/cli.md).

![diffctx demo](https://raw.githubusercontent.com/nikolay-e/diffctx/main/docs/demo/demo.gif)

## How it compares

Whole-repo packers export everything; code-graph servers answer queries
against a maintained index. diffctx is **diff-seeded**: the input is a change,
the output is what it touches and what explains it. Measured results, and when
the other two fit better: [COMPARISON.md](COMPARISON.md).

## More

- [Documentation site](https://diffctx.com/) — the pipeline end to end
- [Command-line reference](docs/product/cli.md) — rendered from `diffctx --help`
- [Integrations](docs/product/integrations.md) — install, MCP clients, Python API
- [FAQ](docs/product/faq.md) — heuristic or oracle, ignore rules, monorepos, secrets
- [Token counting](docs/product/token-budget.md) — `--budget` for non-GPT models
- [GitHub Action](docs/product/github-action.md) — diff context as a CI step
- [Benchmarks](BENCHMARKS.md) and the [paper](https://doi.org/10.5281/zenodo.18824579)
- [Changelog](CHANGELOG.md) · [Security](SECURITY.md)
- [Parameter strategy](docs/engineering/parameter-strategy.md)

Apache 2.0

<!-- mcp-name: io.github.nikolay-e/diffctx -->
<!-- Ownership marker read from the PyPI description by the MCP registry. -->
<!-- Must survive edits verbatim: one space after the colon, case-sensitive. -->
