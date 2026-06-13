# diffctx — QA Playbook

Project-specific QA notes. Generic QA patterns live in the `/qa` skill — do not
duplicate here. In particular, Packaging QA (wheel + clean-venv E2E,
Test-Gating Trap, mypy hook scope, `language: system` venv-shebang rot,
`which`-vs-pipx trap, maturin editable build) lives in the skill's
**Packaging QA** section — this file only records the diffctx-specific shape of
each.

## Applicability Matrix

| Check | Applies | Notes |
|---|---|---|
| CI status (`gh run`) | yes | `diffctx CI`, `CodeQL`, `Dependency Graph` workflows |
| Test suite | yes | `pytest -q`, see Test Suite Layout below |
| Pre-commit | yes | Full suite locally; see Pre-commit Caveats |
| Code review | yes | Diff-mode of own tool: `diffctx --diff <range>` |
| CLI smoke | yes | See CLI Smoke Recipes |
| SonarCloud | no | Project NOT registered on SonarCloud |
| autoqa pipeline | no | CLI tool, no HTTP API surface |
| K8s logs / ArgoCD | no | No deployment to a cluster |
| Browser QA / Walkthrough | no | No UI |
| Schemathesis / ZAP | no | No OpenAPI / HTTP service |
| Backend smoke | no | CLI tool |

## Build & Install Layout

- Python + Rust hybrid wheel built via maturin (PEP 660).
- Editable install: `pip install -e ".[dev,full,mcp]" --no-build-isolation`
  after `pip install "maturin>=1.10,<1.14"`.
- Rust crate lives in `diffctx/` subdir; Python sources in `src/diffctx/`.
- Rust extension module name: `diffctx._diffctx` (from `[tool.maturin]`).

## Test Suite Layout

- pytest-xdist runs tests in parallel (`addopts = "-n auto --dist worksteal"`).
- 337+ pytest tests; 16 of them live in `tests/test_mcp.py` and are gated
  by `pytest.importorskip("mcp")`. They use `pytest-asyncio` with
  `asyncio_mode = auto` (in `pyproject.toml`).
- `tests/test_mcp.py` is the ONLY async-using test module. If you add
  `@pytest.mark.asyncio` decorators elsewhere, the `auto` mode picks them
  up automatically — keep `pytest-asyncio` in `[dev]`.
- 2 legitimate (not stale) conditional skips:
  `test_clipboard.py: Windows only` (skips on macOS/Linux),
  `test_mcp.py: mcp package not installed` (skips when extra is missing).

### Test-Gating Trap — diffctx specifics

See `/qa` skill: Packaging QA (Test-Gating Trap) for the general lesson. Here
it bites at the intersection of two extras: `[mcp]` must be in the CI install
extras (else `importorskip("mcp")` silently skips all 16 `test_mcp.py` tests),
AND `pytest-asyncio` + `asyncio_mode = auto` must be present (else the async
tests collect but never await — they "pass" by never running). Re-check this
every time someone adds a new `[<extra>]` that ships a tool with its own tests.

## CLI Smoke Recipes

```bash
# Tree mode (CLI is also called `diffctx`):
diffctx src/diffctx/mcp --no-content -f yaml

# Diff mode, explicit range:
diffctx --diff HEAD~3..HEAD -f yaml

# Diff mode, bare --diff (defaults to HEAD):
diffctx --diff -f yaml
```

Format flag is `-f / --format`, NOT `--output-format` (common typo).

## Pre-commit Caveats

See `/qa` skill: Packaging QA for `language: system` venv-shebang rot (recover
with `rm -rf .venv && python3 -m venv .venv && pip install "maturin>=1.10,<1.14"
&& pip install -e ".[dev,full,mcp]" --no-build-isolation`). CI is unaffected
(fresh venv per run).

diffctx-specific hygiene: stale `src/treemapper.egg-info/` from a rebrand-era
`pip install` is gitignored but may linger — delete on hygiene pass.

## Diff-Mode Self-Eat

`diffctx --diff <range>` runs on this repo's own history. The tool is its own
test fixture. Use it during code review to surface the same semantic context an
external user would see — large diffs (>10k tokens) are normal for big commits
and are not regressions.

## Local `which diffctx` Trap — diffctx specifics

See `/qa` skill: Packaging QA (`which`-vs-pipx). Concretely: when this project's
venv is active, `/Users/nikolay/diffctx/.venv/bin` sits FIRST on `$PATH`, so a
bare `diffctx ...` inside the working tree runs the working-tree build, NOT the
pipx-published binary. For QA code-review smoke, always use the absolute path
`/Users/nikolay/.local/bin/diffctx`. Tests, builds, and pre-commit need the venv
binary; only the user-facing smoke / review step needs the pipx one.

## Empty-Diff Warning Is Expected on Docs-Only HEAD

`diffctx --diff` (bare, no range → defaults to HEAD) on a docs-only HEAD prints
`diffctx: diff produced no semantic context (pure deletion, binary-only, or
all files exceeded size cap); output empty.` and emits an 11-token YAML
skeleton. Not a regression — this is the actionable-error contract. CLI smoke
check should accept the warning and the empty `fragments:` list, NOT fail on it.

---

Generic QA patterns live in the `/qa` skill — do not duplicate here.
