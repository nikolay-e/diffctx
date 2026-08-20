# Contributing to diffctx

## Getting Started

```bash
git clone https://github.com/nikolay-e/diffctx.git
cd diffctx
rustup component add rustfmt clippy
python -m venv .venv && source .venv/bin/activate
pip install "maturin>=1.10,<1.15"
pip install -e ".[dev,full,mcp]" --no-build-isolation
pre-commit install && pre-commit install --hook-type commit-msg
```

The package builds the Rust extension via maturin, so `maturin` must be
installed first and `--no-build-isolation` is required. `rust-toolchain.toml`
pins the compiler but deliberately not `rustfmt`/`clippy` — pinning components
makes rustup install them before anything can *build* the crate, which breaks
`pip install` from an sdist on a machine that already has them. The
`rustup component add` above is what the pre-commit hooks need.

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
cargo test --lib                                             # Rust inline units
DIFFCTX_YAML_CASES_LIMIT=20 cargo test --test yaml_cases     # sampled YAML corpus
```

CI gates the full 2725-case YAML corpus per-case against
`crates/diffctx-native/tests/known_below_threshold.txt` (bidirectional: a
listed case that starts passing also fails — claim improvements by removing
the entry in the same commit).

## Code Style

- Formatting: `black` (line-length 130)
- Linting: `ruff`
- Type checking: `mypy --strict`
- No docstrings or inline comments explaining "what" — code must be self-documenting

## Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/) — e.g.
`fix: handle empty directories in diff context`. The commit-msg hook enforces
the format.

## Reporting Bugs

Use the [bug report template](https://github.com/nikolay-e/diffctx/issues/new?template=bug_report.yml).

## Security Vulnerabilities

See [SECURITY.md](SECURITY.md).
