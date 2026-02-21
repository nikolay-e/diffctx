# Contributing to TreeMapper

## Getting Started

```bash
git clone https://github.com/nikolay-e/treemapper.git
cd treemapper
python -m venv .venv && source .venv/bin/activate
pip install -e ".[dev,tree-sitter]"
pre-commit install && pre-commit install --hook-type commit-msg
```

## Development Workflow

1. Create a branch: `feature/description` or `fix/description`
2. Make changes
3. Run tests: `pytest`
4. Run linting: `pre-commit run --all-files`
5. Submit a pull request against `main`

## Testing

Integration tests only — no unit tests, no mocking.
Tests run against real filesystems and real git repositories.

```bash
pytest                          # run all tests
pytest -x                       # stop on first failure
pytest tests/test_basic.py      # run specific test file
```

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

Use the [bug report template](https://github.com/nikolay-e/treemapper/issues/new?template=bug_report.yml).

## Security Vulnerabilities

See [SECURITY.md](SECURITY.md).
