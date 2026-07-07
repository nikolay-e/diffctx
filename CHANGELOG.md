# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.11.0] - 2026-07-07

### Changed

- **The default `--format` is now `md` (Markdown), previously `yaml`.** Markdown
  is ~7% more token-efficient than YAML on real diffs and is preferred by
  reviewers for code-fragment-heavy output (evidence:
  `benchmarks/real_world_diff_bench/`). YAML remains available via `-f yaml`.
  This also changes the default stdout format of `treemapper` (a passthrough
  wrapper over this engine); downstream scripts that parse the default output
  must now pass `-f yaml` explicitly. (#104)

## [1.10.0] - 2026-06-20

### Changed

- The default `--tau` (stopping threshold) is now **0.12** across the CLI, MCP,
  and Python API — the calibrated grid optimum — bringing the Python side in
  line with the standalone Rust binary (was 0.08). Diff-context selections are
  slightly tighter by default.
- The MCP `get_tree_map` / `get_file_context` default `max_file_bytes` is now
  256 KB, matching the documented CLI default (was 100 KB).

### Fixed

- Document/citation edges can no longer create self-edges (a fragment linking to
  itself); `Graph::add_edge` now rejects `src == dst`.
- Test→source import edges no longer match `import` statements that appear inside
  comments or string literals (the import regex is now line-anchored).
- The MCP `get_file_context` clipboard confirmation now reports the number of
  files actually copied, excluding files skipped for exceeding the size cap.

### Docs

- Corrected the AST-parsing language count (was "12 languages", now "30+"),
  aligned the Rust crate version with the wheel, completed the MCP server README
  (all three tools documented), and fixed the documented `EGO.per_hop_decay`
  default (0.5). Documented the `scoring_mode` / `timeout` Python API params,
  clarified `--budget` / `--tau` help text, and marked the `DIFFCTX_OP_*` /
  `DIFFCTX_*` tuning env vars as an experimental, non-public interface.

## [1.9.2] - 2026-06-14

### Added

- Private-key and keystore files are now excluded from output in **both** tree
  mapping and `--diff` context, since such material is never legitimate LLM
  context: `*.pem`, `*.key`, `*.pfx`, `*.p12`, `*.keystore`, `*.jks`, and SSH
  private keys `id_rsa`/`id_dsa`/`id_ecdsa`/`id_ed25519` (public `.pub` keys stay
  visible). The `--diff` path previously applied no ignore filtering at all, so a
  changed key file would have leaked into context. Use `--no-default-ignores` to
  opt out of tree-mode default ignores. (`.env` files are intentionally still
  included — a changed `.env` is legitimate change context; redacting secret
  *values* is a separate planned content-scan feature.)

### Fixed

- **Diffs touching files larger than 100 KB no longer produce empty output.** A
  changed file (e.g. a 142 KB `Math.h`) was subject to the same 100 KB cap as
  context-discovery candidates, so the whole diff yielded "no semantic context".
  Changed files — the subject of the diff — are now parsed up to 5 MB.
- **C/C++ symbol names** for variable declarations now report the variable, not
  the type: a changed `const unsigned blane = …` is labeled `blane`, not
  `unsigned`. Covers `init`/`array`/pointer/function declarators and C++
  `reference`/`parenthesized` declarators (`int &r`, `int (*f)()`).
- Changed-file fragments are no longer dropped by the generated-file reduction
  (cap of 5 + 30-line content truncation), which could discard the small
  fragment covering the edited hunk and mislocate the change.
- Binary detection no longer misclassifies changed text files that embed ANSI
  escape / control bytes (snapshot and terminal-recording fixtures); it now uses
  git's NUL-byte heuristic, so such files are no longer silently dropped.
- Diff-context selection is now deterministic across processes: ego-graph score
  accumulation, context-fragment capping, and the greedy selection heap use
  stable tie-breaks, so identical inputs always yield byte-identical output.
- Python module-import edges are bidirectional (matching every other language
  and Python's own symbol references), so a changed imported module now
  propagates relevance back to its importers.

### Hardened

- `git cat-file` blob reads are bounded (drain-and-reject above 16 MB) instead of
  allocating an arbitrarily large buffer up front.

## [1.9.1] - 2026-06-14

> Supersedes 1.9.0, which was tagged but never published to PyPI (release-process
> hiccup); 1.9.1 ships the same changes. There is no 1.9.0 on PyPI.

### Added

- Diff-context output now leads with an orientation header — `commit_message`
  and `changed_files` — in every format (YAML/JSON/Markdown/text), so a reader
  sees *what* changed before reading any fragment.
- Each fragment carries a `role` of `changed` when it overlaps the diff hunks
  (omitted for supporting context). Changed code is emitted first; context
  follows, ordered by descending per-file relevance instead of alphabetically.

### Changed

- Line-contiguous fragments of the same role within a file are merged into a
  single entry, cutting the per-fragment scaffolding that dominated output made
  up of one-line snippets (lossless on line coverage).

### Fixed

- `get_changed_files` and `get_untracked_files` canonicalize paths consistently
  with deleted/discovered files, preventing duplicate fragments when a tracked
  path traverses a symlinked directory.
- Signature extraction no longer terminates at braces inside parameter defaults
  or annotations (e.g. Python `def f(x={}):`), which previously truncated the
  signature mid-parameter-list.
- The post-pass that rescues unrepresented changed files reuses the open
  `git cat-file --batch` reader instead of spawning a `git show` per file.
- Degraded token-count fallback returns 0 for the empty string (was 1).

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
