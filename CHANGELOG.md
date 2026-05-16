# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.7.0] - 2026-05-22

### Added

- README documents the `graph` subcommand and the `--scoring {ppr,ego,bm25}`
  flag (default: `ego`); the absolutist "Uses Personalized PageRank" language
  has been replaced with a table covering all three scoring modes.
- `SECURITY.md` now ships a "Threat model" section that scopes treemapper as a
  local CLI (filesystem + git subprocess, no network) and documents that the
  optional `treemapper-mcp` server confines its filesystem reach via
  `TREEMAPPER_ALLOWED_PATHS`.
- Footer of `README.md` links `CHANGELOG.md`, `SECURITY.md`, and
  `docs/parameter-strategy.md` so they are no longer orphaned.

### Changed

- Single source of truth for the package version: the Python wheel reads its
  version from `Cargo.toml` (via maturin) instead of duplicating it in
  `pyproject.toml`, eliminating the Rust-crate-vs-Python-package version
  desync that previously shipped to PyPI.
- `--diff` now defaults to `HEAD` when no range is supplied, matching the
  most common invocation (`treemapper . --diff` → diff of the working tree
  against `HEAD`) and saving an argument in the 30-second demo path.
- `numpy` moved from a required runtime dependency into the `[tree-sitter]`
  extra, so default installs no longer pull a ~20 MB scientific stack the
  core tree-mapping mode does not use.
- CLI error messages are now actionable: instead of a raw Python traceback,
  invalid `--diff` ranges, missing git repositories, and unreadable paths
  print a one-line `Error: <what> — try: <next step>` and exit with code `2`
  for user-input errors (`1` is reserved for runtime failures).
- `automerge.yml` GitHub Actions workflow hardened: explicit minimal
  `permissions:` block, pinned action SHAs, and a guard that refuses to
  auto-merge anything touching `.github/`, `pyproject.toml`, or `Cargo.toml`.

### Fixed

- Replaced every user-reachable `unwrap()`/`expect()` in the Rust core
  (`tokenizer.rs`, `git.rs`, `scoring.rs`, `pybridge.rs`) with proper
  `PyRuntimeError` / `GitError` propagation. A malformed diff, a missing
  BPE table, or an oversized hunk-header integer no longer aborts the
  Python interpreter via `panic = "abort"`.
- `treemapper-mcp` entry point now guards against being launched without the
  `[mcp]` extra installed and prints an install hint instead of an
  `ImportError` traceback.

### Removed

- Dropped Kotlin and F# from the language matrix: tree-sitter grammars for
  both were silently misaligned with the project's import-resolution rules
  and produced misleading edge weights. They will return once the grammars
  are vetted.

### Security

- MCP server (`treemapper-mcp`) now refuses to traverse outside the
  directories listed in `TREEMAPPER_ALLOWED_PATHS` (OS-pathsep-separated)
  and refuses to start if the envvar is unset when run as a network-facing
  process. See [`SECURITY.md`](SECURITY.md) for the threat model.

## [1.6.1] - 2026-05-15

### Fixed

- **`_diffctx` Rust extension was missing from PyPI wheels** — `pip install treemapper`
  previously shipped a pure-Python wheel that lacked the compiled Rust core, causing
  `treemapper . --diff HEAD~1..HEAD` (the headline diff-context mode) to crash with
  `ModuleNotFoundError: No module named '_diffctx'`. The build backend is now
  `maturin`, and CD builds and publishes ABI3 wheels for Linux (x86_64 + aarch64),
  macOS (x86_64 + arm64) and Windows (x86_64). Token counting (`tokens.py`) and
  language detection (`writer.py`) now use the Rust path on every install instead
  of silently degrading.
- CI no longer references three deleted test files (`tests/test_graph*.py`) via
  `--ignore` flags.

### Added

- Post-publish CD smoke job installs the freshly published wheel from PyPI on a
  clean runner and exercises `treemapper --version`, the tree-mapping mode, the
  Rust extension import (`from treemapper._diffctx import …`) and the diff-context
  mode end-to-end across {Linux, macOS, Windows} × Python {3.10, 3.13}. The
  release fails (and is not finalized as a GitHub Release) if the smoke test
  fails — preventing a recurrence of the silent-degradation defect above.
- Output-size warning: when an emitted tree-mapping output exceeds 10 MB, a
  stderr hint suggests using `--no-content` (structure only) or `--diff RANGE`
  (relevance-ranked context).
- `CHANGELOG.md` (Keep-a-Changelog 1.1.0 format).

### Changed

- `--max-file-bytes` default lowered from 10 MB to 256 KB to keep tree-mapping
  output bounded on large repos (cpython would previously produce ~133 MB of
  YAML with the old default). Use `--no-file-size-limit` to disable the
  per-file cap.
- `from _diffctx import …` paths refactored to `from treemapper._diffctx import …`
  (the Rust extension is now a submodule of the `treemapper` package). External
  consumers of the Python API are unaffected; this is purely an internal layout
  change required by the unified maturin wheel.

### Removed

- `Programming Language :: Python :: 3.14` classifier (CI runs 3.14 as
  best-effort only; classifier was premature).
- Unused optional-dependency groups `[embeddings]` (sentence-transformers) and
  `[nlp]` (spacy) — neither was imported anywhere in `src/`.
- Stale paper-track scaffolding: `PAPER_DEVIATIONS.md`, `QA.md`,
  `Dockerfile.bench`, `requirements-bench.{txt,lock}`, `sonar-project.properties`,
  `treemapper.spec` (PyInstaller spec), `whitelist_vulture.py`.
- PyInstaller binary build path in CD (was orphan after the Rust extension
  began shipping via wheels; binary distribution is deferred).

## [1.6.x and earlier]

Release notes for `1.0.0` through `1.6.0` are published on GitHub:
<https://github.com/nikolay-e/diffctx/releases>.

[Unreleased]: https://github.com/nikolay-e/diffctx/compare/v1.7.0...HEAD
[1.7.0]: https://github.com/nikolay-e/diffctx/releases/tag/v1.7.0
[1.6.1]: https://github.com/nikolay-e/diffctx/releases/tag/v1.6.1
