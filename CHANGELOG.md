# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.8.0]

### Added

- Public `diffctx.run(argv=None, *, prog=None, version=None)` engine entry. It
  is the same execution path as the `diffctx` console script but accepts an
  injected program name and version string, so a downstream wrapper (e.g. the
  `treemapper` distribution) can present its own branding in `--help`,
  `--version`, and error prefixes without duplicating the CLI. `main()` now
  delegates to `run()` with the default `diffctx` identity — no behavior change
  for existing callers.
- `diffctx.mcp.__main__.main(prog=..., extra=...)` accepts an injected program
  name and install-extra string for the missing-dependency hint, so wrappers
  can re-expose the MCP entry point with their own name.

## [1.7.0] - 2026-05-22

### Added

- README documents the `graph` subcommand and the `--scoring {ppr,ego,bm25}`
  flag (default: `ego`); the absolutist "Uses Personalized PageRank" language
  has been replaced with a table covering all three scoring modes.
- `SECURITY.md` now ships a "Threat model" section that scopes diffctx as a
  local CLI (filesystem + git subprocess, no network) and documents that the
  optional `diffctx-mcp` server confines its filesystem reach via
  `DIFFCTX_ALLOWED_PATHS`.
- Footer of `README.md` links `CHANGELOG.md`, `SECURITY.md`, and
  `docs/parameter-strategy.md` so they are no longer orphaned.

### Changed

- **Package renamed from `treemapper` to `diffctx`.** PyPI distribution, CLI
  binary, MCP server binary, and Python import path all use `diffctx`. The
  `treemapper` PyPI package remains at 1.6.1 (frozen); install
  `diffctx` for all new development. The `TREEMAPPER_ALLOWED_PATHS`
  environment variable is now `DIFFCTX_ALLOWED_PATHS`.
- Single source of truth for the package version: the Python wheel reads its
  version from `Cargo.toml` (via maturin) instead of duplicating it in
  `pyproject.toml`, eliminating the Rust-crate-vs-Python-package version
  desync that previously shipped to PyPI.
- `--diff` now defaults to `HEAD` when no range is supplied, matching the
  most common invocation (`diffctx . --diff` → diff of the working tree
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
- `diffctx-mcp` entry point now guards against being launched without the
  `[mcp]` extra installed and prints an install hint instead of an
  `ImportError` traceback.

### Removed

- Dropped Kotlin and F# from the language matrix: tree-sitter grammars for
  both were silently misaligned with the project's import-resolution rules
  and produced misleading edge weights. They will return once the grammars
  are vetted.

### Security

- MCP server (`diffctx-mcp`) now refuses to traverse outside the
  directories listed in `DIFFCTX_ALLOWED_PATHS` (OS-pathsep-separated)
  and refuses to start if the envvar is unset when run as a network-facing
  process. See [`SECURITY.md`](SECURITY.md) for the threat model.

## [1.6.1 and earlier]

Earlier releases shipped as `treemapper`; see
<https://pypi.org/project/treemapper/#history> for legacy versions and
<https://github.com/nikolay-e/diffctx/releases> for the corresponding GitHub
release notes (`1.0.0` through `1.6.1`).

[Unreleased]: https://github.com/nikolay-e/diffctx/compare/v1.7.0...HEAD
[1.7.0]: https://github.com/nikolay-e/diffctx/compare/v1.6.1...v1.7.0
