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

## Secret-Handling Test Fixtures Break the Secret Hooks

The private-key exclusion tests (`test_secret_ignores_diff.py`,
`test_default_ignores.py`) assert that diffctx drops key/keystore files. Both the
Rust `is_secret_path` and the Python `ignore.py` match **by filename only**
(`id_rsa`, `*.pem`, `*.key`, …) — the fixture content is irrelevant to what they
test. So fixtures must NOT embed a literal PEM `BEGIN…PRIVATE KEY` banner:
`detect-private-key` (no pragma support) and `detect-secrets` both flag it, and a
file committed past local hooks (e.g. `--no-verify`) then turns `Pre-commit
hooks` + `Lint & Type Check` red on `--all-files` while a 20-case CI YAML subset
stays green. Use inert content (`"private-key-material <MARKER>\n"`) plus
`# pragma: allowlist secret` for the entropy detector; keep distinctive leak
markers (`LEAK_RSA`, …) so leakage is still detectable. High-entropy base64
findings come from concatenating tokens with no separator — keep a space.

Catch this class only with the FULL local suite: `pre-commit run --all-files`
(NOT a staged-files commit run, which skips clean files). When backgrounding it,
note the shell exit code is the trailing `echo`'s, not pre-commit's — grep the
log for `Failed`, don't trust the reported exit.

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

## YAML Cases: CI Runs Only the First 20

`ci.yml` sets `DIFFCTX_YAML_CASES_LIMIT: "20"`, so `cargo test --test yaml_cases`
in CI runs only the first 20 discovered cases (sorted by filename). Everything
alphabetically after that — `kubernetes_*`, later `bal2_*`, etc. — **never runs
in CI**. A green CI does NOT mean the full case suite passes. On every QA pass,
run the FULL suite locally (`cd diffctx && cargo test --release --test yaml_cases`)
and triage cases below the score threshold (default `min_score=10`, override via
`DIFFCTX_YAML_MIN_SCORE`). A failing case scores `100 * recall * (1 - forbidden_rate)`;
`forbidden_rate=100%` means the selection pulled in every "unrelated" manifest.

## Edge-Builder Regexes: Compile-Probe, Not Pipeline Coverage

The diff pipeline does NOT force every edge-builder `Lazy<Regex>` for small test
inputs (PPR can pull a fragment in via a structural edge without ever invoking
the k8s selector/label extraction). So a YAML case that "passes" does not prove
the edge regexes even compile. A nested bounded repetition of a Unicode negated
class — `(?:...[^\n:]{1,200}\n){1,50}` — compiles past regex's default 10 MiB
limit and `.unwrap()` aborts the process the first time it's forced (a real k8s
repo via the CLI hits it; the test suite did not). The deterministic guard is a
`#[cfg(test)]` probe that forces every regex in the module to compile
(`kubernetes::tests::all_kubernetes_regexes_compile`). Unbound the value class
(`[^\n:]+`) instead of bounding it — the regex crate is linear-time, so no ReDoS.

## Building the Extension: maturin, Not `cargo build --features python`

`cargo build --features python` fails at the link step (`linking with cc failed`,
undefined Python symbols) because the `extension-module` pyo3 feature expects the
host interpreter to provide symbols at import time. To compile-check the Python
bridge, use `.venv/bin/python -m maturin develop --release` (or `cargo build`
WITHOUT `--features python` / `cargo test --lib` for the pure-Rust paths).

## Empty-Diff Warning Is Expected on Docs-Only HEAD

`diffctx --diff` (bare, no range → defaults to HEAD) on a docs-only HEAD prints
`diffctx: diff produced no semantic context (pure deletion, binary-only, or
all files exceeded size cap); output empty.` and emits an 11-token YAML
skeleton. Not a regression — this is the actionable-error contract. CLI smoke
check should accept the warning and the empty `fragments:` list, NOT fail on it.

---

Generic QA patterns live in the `/qa` skill — do not duplicate here.
