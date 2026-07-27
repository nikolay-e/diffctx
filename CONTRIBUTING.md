# Contributing to diffctx

## Getting Started

```bash
git clone https://github.com/nikolay-e/diffctx.git
cd diffctx
python -m venv .venv && source .venv/bin/activate
pip install "maturin>=1.10,<1.11"
pip install -e ".[dev,full,mcp]" --no-build-isolation
pre-commit install && pre-commit install --hook-type commit-msg
```

The package builds the Rust extension via maturin, so `maturin` must be
installed first and `--no-build-isolation` is required.

## Development Workflow

1. Create a branch: `feature/description` or `fix/description`
2. Make changes
3. Run tests: `pytest` (and `cargo test --lib` for Rust changes)
4. Run linting: `pre-commit run --all-files`
5. Submit a pull request against `main`

## Testing

The Python suite is integration-only — no mocking, real filesystems and
real git repositories. The Rust core carries inline `#[test]` units plus
a YAML-based declarative integration suite.

```bash
pytest                          # run all Python tests
pytest -x                       # stop on first failure
pytest tests/test_basic.py      # run specific test file

cd crates/diffctx-native
DIFFCTX_YAML_CASES_LIMIT=20 cargo test --lib          # Rust units + sampled YAML cases
```

The full YAML suite (no limit) carries a known score-threshold failure
baseline; CI gates a 20-case sample on PRs and the full run nightly.

## Code Style

- Formatting: `black` (line-length 130)
- Import sorting: `isort`
- Linting: `ruff`
- Type checking: `mypy --strict`
- No docstrings or inline comments explaining "what" — code must be self-documenting

## Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```text
feat: add support for Ruby parsing
fix: handle empty directories in diff context
chore(deps): bump pathspec to 0.12
```

## Reporting Bugs

Use the [bug report template](https://github.com/nikolay-e/diffctx/issues/new?template=bug_report.yml).

## Security Vulnerabilities

See [SECURITY.md](SECURITY.md).
