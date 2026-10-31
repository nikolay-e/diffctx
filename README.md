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

diffctx is a command-line tool; the MCP server, the Claude Code plugin and the
GitHub Action run the same engine.

### CLI

```bash
uvx diffctx . --diff HEAD~1             # zero-install, run once via uv
pipx install diffctx                    # isolated CLI
pip install diffctx                     # into an active environment
cargo install diffctx                   # native binary from crates.io, no Python
npx diffctx . --diff HEAD~1             # npm wrapper over the native binary
docker run --rm -v "$PWD:/repo" ghcr.io/nikolay-e/diffctx . --diff HEAD~1
```

```powershell
scoop bucket add diffctx https://github.com/nikolay-e/diffctx   # Windows
scoop install diffctx/diffctx
```

The native binary — cargo, npm, Docker, Scoop and the
[release archives](https://github.com/nikolay-e/diffctx/releases/latest) for
Linux, macOS and Windows on x64 and ARM — answers `--diff` and `--symbol` in
every mode; whole-tree export, Markdown packs, `diffctx graph` and the MCP
server need the Python package.

### Any MCP client

Registered as `io.github.nikolay-e/diffctx`.

```bash
claude mcp add diffctx -- uvx --from 'diffctx[mcp]' diffctx-mcp
codex mcp add diffctx -- uvx --from 'diffctx[mcp]' diffctx-mcp
gemini mcp add diffctx uvx -- --from 'diffctx[mcp]' diffctx-mcp
```

Cursor, VS Code, Claude Desktop, Windsurf, Zed, Continue and the Python API:
[integrations](docs/product/integrations.md).

### Claude Code plugin

The MCP server plus `/diffctx:impact` and hooks that run it on each commit and
push.

```bash
claude plugin marketplace add nikolay-e/diffctx
claude plugin install diffctx@diffctx
```

### CI

The [GitHub Action](docs/product/github-action.md) hands a pull request's diff
context to any review step.

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

## Language tiers

What an answer promises depends on the file's language. Where diffctx cannot
resolve a reference it says so; it never prints a zero for a language it does
not model.

| Tier | Languages | What an answer promises |
| --- | --- | --- |
| 1 | Python, TypeScript/JavaScript, Rust, Kotlin | Definitions, callers and the tests that reach them, resolved: the languages the engine is being made exact for |
| 2 | Go, Java, Swift, SQL, shell, YAML/k8s/Helm/Argo, HTML/Astro | Candidates, each marked with its evidence; exact answers follow Tier 1 |
| 3 | Every other language | Not modelled: the answer says so, names the files that mention the name, and never claims that nothing calls or reaches it |

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
- [Benchmarks](BENCHMARKS.md) and the [paper](https://doi.org/10.5281/zenodo.18824579)
- [Changelog](CHANGELOG.md) · [Security](SECURITY.md)
- [Parameter strategy](docs/engineering/parameter-strategy.md)

Apache 2.0

<!-- mcp-name: io.github.nikolay-e/diffctx -->
<!-- Ownership marker read from the PyPI description by the MCP registry. -->
<!-- Must survive edits verbatim: one space after the colon, case-sensitive. -->
