# LEVERAGE — diffctx

Open leverage backlog (simplify · DRY · modernize · cut). Each item
carries the gate that must pass before applying; applied or rejected
items are removed rather than logged — history lives in git.

## VERIFY — do iff the named gate passes

- **`once_cell::sync::Lazy` → `std::sync::LazyLock`** — crate is
  edition 2024 / `rust-version 1.85`, `LazyLock` is stable since 1.80,
  all uses are `sync::Lazy`. Gate: clean `cargo build --all-features`
  plus an E-class equivalence double-run. Net: −1 dependency.
- **Vestigial `tree-sitter` Python extra (17 wheels)** — parsing is
  fully in the Rust crates; `rg 'import tree_sitter' src tests eval
  scripts` → 0. Gate: confirm no downstream consumer installs
  `diffctx[tree-sitter]`/`[full]` for a Python code path, then drop the
  group and its `full`/`dev` self-references.
- **`black` → `ruff format`** — both pinned to line-length 130; ruff
  already owns import sorting. Pure consolidation. Gate:
  `ruff format --check` diff-clean against current black output.
- **~40 near-identical `is_<lang>_file` helpers**
  (`crates/diffctx-native/src/edges/semantic/*.rs`) — collapse onto a
  shared `base::has_ext` helper. Gate: E-class equivalence run.
  Net: −60…80 LOC.
- **Parametrize the default-ignore test matrix**
  (`tests/test_complete_coverage.py`, `test_default_ignores.py`,
  `test_coverage_gaps.py`) — ~20 copy-paste methods → one
  `@pytest.mark.parametrize`. Gate: the three files stay green.
  Net: ~−290 LOC, zero coverage loss.
- **`eval/workflows/backfill_checkpoints.py`** — one-off migration for
  pre-field-stamping checkpoints; zero references. Gate: confirm the
  evaluator stamps every field `cell_metrics.py` reads (checkable any
  time — the paper-v2 sweep is frozen), then delete. Net: −142 LOC.
- **`fn _used` no-op in `pybridge.rs`** — a no-op fn cannot suppress
  field warnings. Gate: build stays warning-clean after removal.
- **`serde_yaml 0.9` is archived** — used in 3 files. Gate: test-suite
  parity on a maintained fork (`serde_norway`/`serde_yml`), or
  consciously pin-and-accept.
- **`mcp/formatting.py`** — 9-line single-function indirection with one
  caller. Gate: `pytest tests/ -k mcp` green after inlining.

## Intentional — do not "fix"

- `DEFAULT_MAX_FILE_BYTES` duplicated across `cli.py` and `mcp/server.py`:
  the import-linter layering contract forbids mcp importing cli, and the
  engine does not own a file-byte cap to read it from. (`_DEFAULT_TAU`,
  `_DEFAULT_ALPHA`, the scoring-mode list and the pipeline timeout were on
  this list and are no longer duplicated — all four are read from the
  extension, which both layers already sit on.)
- `compute_scored_state` / `select_with_params` /
  `clipboard_available`: product-dead but load-bearing for the eval
  harness and test skip-guards.
- `eval/` research scaffolding: reachable via `python -m eval` and CI,
  required to reproduce the frozen paper-v2 results.

## Known audit gap

Rust dependency deadness was verified with `rg` only —
`cargo-machete` was unavailable. Re-run with it installed for
exhaustive coverage.
