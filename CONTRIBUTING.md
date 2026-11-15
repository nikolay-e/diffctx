# Contributing to diffctx

## Getting Started

```bash
git clone https://github.com/nikolay-e/diffctx.git
cd diffctx
rustup component add rustfmt clippy
uv sync --locked --no-build --extra dev --extra full --extra mcp
source .venv/bin/activate
pre-commit install && pre-commit install --hook-type commit-msg
```

`uv.lock` is the resolved dependency set every CI job installs, via this exact
command — `--locked` fails instead of re-resolving, so a pyproject edit that was
not locked cannot reach a green build. Regenerate with `uv lock` and commit the
result in the same change. `--no-build` takes every dependency from a wheel, so
no third-party `setup.py` executes during install; the root project is exempt
and still compiles through maturin, which is the one build this repo means to
run.

The package builds the Rust extension via maturin; uv provisions the build
backend in its own isolated environment, so nothing has to be installed before
the sync. `rust-toolchain.toml` pins the compiler but deliberately not
`rustfmt`/`clippy` — pinning components makes rustup install them before
anything can *build* the crate, which breaks installing from an sdist on a
machine that already has them. The `rustup component add` above is what the
pre-commit hooks need.

## Development Workflow

Contributors submit a pull request against `main`. Maintainers work on `next`
and deliver through `scripts/epoch-push.sh`, as described in `AGENTS.md`.
Before delivery, run the tests below and `pre-commit run --all-files`.

A pull request with a `fix:` commit that touches engine source (`src/`,
`crates/*/src/`) carries a class test and states it in the `Class test:` field
of the template. CI builds the pull request's base, and the test must fail
there and pass on the head. The exception is a false "all clear", a leak or a
broken delivery, written `Class test: none — truth|leak|delivery gh#N`.

## Testing

The Python suite is integration-only — no mocking, real filesystems and
real git repositories. The Rust core carries inline `#[test]` units plus
a YAML-based declarative integration suite.

```bash
pytest                          # run all Python tests

scripts/cargo-test-suites.sh                                  # every Rust suite, as CI runs them
DIFFCTX_YAML_CASES_LIMIT=48 scripts/cargo-test-suites.sh      # with a sampled YAML corpus
```

## Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/) — e.g.
`fix: handle empty directories in diff context`. The commit-msg hook enforces
the format.

## Reporting Bugs

Use the [bug report template](https://github.com/nikolay-e/diffctx/issues/new?template=bug_report.yml).
