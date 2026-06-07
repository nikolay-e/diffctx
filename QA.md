# diffctx — QA Playbook

Project-specific QA notes. Generic patterns (pre-commit, SonarCloud rules,
git workflow) live in the global QA skill — do not duplicate here.

## Applicability Matrix

| Check | Applies | Notes |
|---|---|---|
| CI status (`gh run`) | yes | `diffctx CI`, `CodeQL`, `Dependency Graph` workflows |
| Test suite | yes | `pytest -q`, see Test Suite Layout below |
| Pre-commit | yes | Full suite locally; see Pre-commit Caveats |
| Code review | yes | Diff-mode of own tool: `diffctx --diff <range>` |
| CLI smoke | yes | See CLI Smoke Recipes |
| SonarCloud | no | Project NOT registered on SonarCloud as of 2026-05-17 |
| autoqa pipeline | no | CLI tool, no HTTP API surface |
| K8s logs / ArgoCD | no | No deployment to a cluster |
| Browser QA / Walkthrough | no | No UI |
| Schemathesis / ZAP | no | No OpenAPI / HTTP service |
| Backend smoke | no | CLI tool |

## Build & Install Layout

- Python + Rust hybrid wheel built via maturin (PEP 660).
- Editable install: `pip install -e ".[dev,full,mcp]" --no-build-isolation`
  after `pip install "maturin>=1.10,<1.14"`.
- Local venv at `/Users/nikolay/diffctx/.venv`. CI builds fresh env per job.
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
- 2 Windows-only / mcp-only conditional skips are legitimate, not stale:
  `test_clipboard.py:35: Windows only` (skips on macOS/Linux),
  `test_mcp.py: mcp package not installed` (skips when extra is missing).

### Test Gating Trap (2026-05-17 incident)

Before this QA pass, `test_mcp.py` was completely silent in CI:

- CI installed `[dev,full]` but NOT `[mcp]` → `importorskip` skipped the module.
- Locally, even when `[mcp]` was installed, every test failed because
  `pytest-asyncio` was not declared anywhere and `@pytest.mark.asyncio` was
  ignored → tests collected but never awaited.
- Net effect: 16 MCP tests appeared to "pass" everywhere because they
  never ran.

Fix landed in commit `8abd915d`: added `pytest-asyncio` to `[dev]`,
`asyncio_mode = auto` to pytest ini_options, and `[mcp]` to CI install
extras. Re-check the Test Gating Trap pattern every time someone adds a
new `[<extra>]` that ships a tool with its own tests.

## CLI Smoke Recipes

```bash
# Tree mode (CLI is also called `diffctx`):
diffctx src/diffctx/mcp --no-content -f yaml

# Diff mode, explicit range:
diffctx --diff HEAD~3..HEAD -f yaml

# Diff mode, bare --diff (defaults to HEAD — feature from commit f67efab0):
diffctx --diff -f yaml
```

Format flag is `-f / --format`, NOT `--output-format` (common typo).

## Pre-commit Caveats

- `language: system` hooks (mypy, pip-audit, import-linter) call binaries
  via execve — they hit the shebang directly. If the local venv was created
  before a directory rename (`~/treemapper` → `~/diffctx`), every shebang
  inside `.venv/bin/*` will point to `~/treemapper/.venv/bin/python` and
  fail with `Executable not found`. CI is unaffected (fresh venv per run).

  Recovery:

  ```bash
  rm -rf .venv && python3 -m venv .venv
  source .venv/bin/activate
  pip install "maturin>=1.10,<1.14"
  pip install -e ".[dev,full,mcp]" --no-build-isolation
  ```

- Generated artifacts left behind by the rebrand:
  - `src/treemapper.egg-info/` — gitignored, but stale `pip install` may
    leave it. Delete on hygiene pass.

## Sonar Status

- No `sonar-project.properties`, no `SonarCloud Scan` step in CI.
- The "Upload coverage for SonarCloud" step in `.github/workflows/ci.yml`
  uploads an artifact with 1-day retention but has no consumer — this is
  dead infrastructure, kept on the assumption Sonar will be wired up later.
- Sonar API confirms `nikolay-e_diffctx` and `nikolay-e_treemapper` both
  return "Project not found" (badge endpoint).

## Repo Rename Note

GitHub repo was renamed from `nikolay-e/treemapper` to `nikolay-e/diffctx`.
GitHub auto-redirects but the local origin must be updated:
`git remote set-url origin git@github.com:nikolay-e/diffctx.git`.

## Diff-Mode Self-Eat

`diffctx --diff <range>` runs on this repo's own history. The tool is the
test fixture. Use it during code review to surface the same semantic
context an external user would see — large diffs (>10k tokens) are normal
for rebrand-class commits and should not be treated as regressions.

## Local `which diffctx` Trap

`/Users/nikolay/diffctx/.venv/bin` sits FIRST on `$PATH` when this project's
venv is active (which happens automatically after the recovery flow above).
That means a bare `diffctx ...` inside the working tree runs the working-tree
build, NOT the pipx-published binary. For QA code-review steps, always use
the absolute path `/Users/nikolay/.local/bin/diffctx` so the run matches what
external users get. Tests, builds, and pre-commit need the venv binary; only
the user-facing smoke / review step needs the pipx one.

## Empty-Diff Warning Is Expected on Docs-Only HEAD

`diffctx --diff` (bare, no range → defaults to HEAD per commit `f67efab0`)
on a docs-only HEAD prints
`diffctx: diff produced no semantic context (pure deletion, binary-only, or
all files exceeded size cap); output empty.` and emits a 11-token YAML
skeleton. Not a regression — this is the actionable-error contract from
`f67efab0`. CLI smoke check should accept the warning and the empty
`fragments:` list, NOT fail on it.
