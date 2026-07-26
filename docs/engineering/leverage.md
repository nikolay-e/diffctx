# LEVERAGE — diffctx

Cumulative leverage-audit log (simplify · DRY/reuse · modernize · restructure ·
clean). Append-only, changelog-style. Each section = one `/review-leverage` run.

## 2026-07-19 — 34872c1b

Scope: full repo (Python `src/`, Rust crate `crates/diffctx-native/src/`, `eval/`,
`scripts/`, `tests/`, tooling/CI). Maturin/pyo3: heavy logic lives in Rust
(`diffctx._diffctx`), Python is a thin CLI/MCP/tree wrapper. Topology: 6
parallel scouts (Python pkg, Rust crate, eval/scripts, tooling/CI,
detectors, tests) → adversarial verification of DO findings → this verdict.
First run; no prior section to reconcile against.

Headline: the codebase is unusually clean. Biggest wins are **phantom config**
(config referencing modules/tools that no longer exist) and a handful of
micro-deletions. Most "suspicious" weight (test-repos corpora, caches, orphaned
`.pyc`) is already gitignored and carries **zero repo weight**. Detector caveat:
`cargo-machete` was unavailable, so Rust deps were audited manually (rg), not
exhaustively.

---

### DO — applied

The full DO batch of this run (phantom `[tool.mutmut]` gate, phantom mypy
overrides, `_normalize_budget` dedup, unused `smallvec` + duplicate
`serde_yaml` dev-dep, dead `numpy` extra, `DiffContextTimeoutError`,
micro-cuts) was applied and verified — items removed from this log.

---

### VERIFY — do iff the named gate passes

## [🟡] Rust: `once_cell::sync::Lazy` → `std::sync::LazyLock`

Category: Modernize · Location: `crates/diffctx-native/Cargo.toml` + ~74 `.rs` files (`use
once_cell::sync::Lazy;`) · Evidence: crate is edition 2024 / `rust-version =
"1.85"`; `LazyLock` is stable since 1.80; no `unsync`/`OnceCell` uses exist (all
uses are `sync::Lazy`). · Problem: an external crate for what std now provides.
· Recommendation: mechanical `Lazy`→`LazyLock` rename, drop the `once_cell` dep.
· Expected benefit: −1 crate, ~148 import/type lines churned (net LOC ≈ flat). ·
Effort: Medium · Confidence: High it's a drop-in (both `Deref`-based). · verdict
**VERIFY(gate: `cd crates/diffctx-native && cargo build --all-features` clean +
equivalence_gate double-run — E-class change per CLAUDE.md, must not alter
selection output)**.

## [🟡] Vestigial `tree-sitter` Python optional-deps group (17 wheels)

Category: Delete / dep surface · Location: `pyproject.toml:91-109` (+ `full` and
`dev` self-refs to `diffctx[tree-sitter]`) · Evidence: `rg 'import
tree_sitter|from tree_sitter' src tests eval scripts` → 0 — parsing is
fully migrated to the Rust `tree-sitter-*` crates (`crates/diffctx-native/Cargo.toml`). The 17
Python `tree-sitter-*` wheels are imported nowhere; only doc mentions remain. ·
Problem: an advertised extra that installs 17 wheels for a code path that no
longer exists in Python. · Recommendation: drop the `tree-sitter` group (and its
`full`/`dev` self-references). · Expected benefit: −19 lines, much smaller
optional install. · Effort: Medium (published-package API surface change) ·
Confidence: Medium · verdict **VERIFY(gate: confirm no downstream consumer
installs `diffctx[tree-sitter]`/`[full]` for a Python code path — grep
issues/docs; if none, remove)**.

## [🔵] `black` alongside `ruff` — consolidate to `ruff format`

Category: Modernize / over-tooling · Location: `pyproject.toml:138-145`,
`.pre-commit-config.yaml` (psf/black hook), `.github/workflows/ci.yml:73-74`
(`black --check`) · Evidence: both black and ruff pinned to `line-length=130`;
ruff already owns import-sorting (`I` in `lint.select`), so isort is not a
separate tool (good). Formatting is still done by black while `ruff format` is a
documented black drop-in; no `[tool.ruff.format]` config exists. · Problem: two
formatters + two pins for one job. · Premise check: black is mature and working
— this is pure consolidation, not a correctness fix. · Recommendation:
optionally drop black, add ruff-format. · Effort: Medium · Confidence: Medium ·
verdict **VERIFY(gate: `ruff format --check` is diff-clean vs current black
output before switching)**.

## [🔵] Rust: ~40 near-identical `is_<lang>_file` helpers

Category: DRY · Location: `crates/diffctx-native/src/edges/semantic/*.rs` (e.g.
`python.rs:15`, `go.rs:15`, `elixir.rs:13`) · Evidence: bodies are either
`EXTS.contains(...)` or inline `ext == ".x"` compares, repeated per language
module. · Recommendation: add `base::has_ext(path, &SET)` /
`base::ext_matches(path, &[..])` and delete the wrappers. · Expected benefit:
−60…80 LOC. · Effort: Medium (unify signature — some callers pass a const set,
some inline literals) · Confidence: High · verdict **VERIFY(gate: E-class
equivalence run — edge extraction output must be identical)**.

## [🔵] Test duplication — parametrize the default-ignore matrix

Category: DRY (tests) · Location: `tests/test_complete_coverage.py:12-301` ·
Evidence: `TestDefaultIgnorePatterns` is 20 near-identical methods (mkdir
project → mkdir ignored dir → write files → `map_directory` → assert not-in/in)
— pure copy-paste table data. Premise check: these assert REAL behaviour (not
coverage padding) — keep the coverage, cut the boilerplate. · Recommendation:
collapse to one `@pytest.mark.parametrize`. Also fold the
`node_modules`/`venv`/cache repeats duplicated in
`tests/test_default_ignores.py:145-165` (keep the one CLI smoke +
`.pyc/.pyo/.pyd/.egg-info` cases it uniquely covers), and fold
`test_coverage_gaps.py:40-89` subset into its superset. · Expected benefit:
~−290 LOC, zero coverage loss. · Effort: Medium · Confidence: High · verdict
**VERIFY(gate: `pytest tests/test_complete_coverage.py
tests/test_default_ignores.py tests/test_coverage_gaps.py` green after
refactor)**.

## [🔵] `eval/workflows/backfill_checkpoints.py` — one-off legacy migration

Category: Delete · Location: `eval/workflows/backfill_checkpoints.py:1` · Evidence:
docstring "Enrich existing checkpoint.jsonl files … Old sweep artifacts predate
the evaluator stamping n_gold, gold_to_changed_ratio …"; 142 LOC, zero refs (no
CI, no `__main__` dispatch, no RESUME.md, no import). · Problem: backfill for
artifacts predating current field-stamping. · Recommendation: delete once
obsolete. · Expected benefit: −142 LOC. · Effort: Easy · Confidence: Medium ·
verdict **VERIFY(gate: confirm the `sweep_v2_local` evaluator stamps
every field `cell_metrics.py` reads, so no legacy checkpoint needs backfilling —
then delete)**. The paper-v2 sweep is finished and frozen (tags `paper-v2`,
`paper-v2-freeze`), so the gate is checkable at any time.

## [🔵] Rust: cargo-cult no-op `_used` fn; `serde_yaml` unmaintained

Category: Cut / modernize · Confidence: Medium · verdict VERIFY (each):

- `crates/diffctx-native/src/pybridge.rs:781-783` — `#[allow(dead_code)] fn _used(_: PathBuf)
  {}` with
comment "silence unused warnings for pyclass getter fields". A no-op fn cannot
suppress field warnings; the build is warning-clean without it. Delete (−3 LOC).
VERIFY(gate: `cargo build --all-features --features python` still warning-clean
after removal).
- `crates/diffctx-native/Cargo.toml:110` — `serde_yaml 0.9` is archived/unmaintained (dtolnay
  deprecated
it 2024), used in 3 files. VERIFY(gate: test-suite parity on a maintained fork
such as `serde_norway`/`serde_yml`) or consciously pin-and-accept.

## [🔵] `mcp/formatting.py` — single-line indirection module

Category: Cut · Location: `src/diffctx/mcp/formatting.py` (9 lines) · Evidence:
`format_diff_context_as_markdown` = `tree_to_string(result, "md")`, one caller
(`server.py:89`). · Recommendation: inline at the call site, delete the module +
import (−9 LOC / −1 file). · Effort: Easy · Confidence: High · verdict
**VERIFY(gate: `pytest tests/ -k mcp` green)**.

---

### DON'T — explicitly rejected

- **`test-repos/` corpora, `target/`, `.mypy_cache/`, `.ruff_cache/`,
`.hypothesis/`, `.pytest_cache/`, `.playwright-mcp/`,
`.import_linter_cache/treemapper.meta.json`, and the ~30 orphaned `.pyc` in
`src/diffctx/_native/__pycache__/`** — DON'T(all verified **gitignored / 0
tracked** via `git ls-files`; zero repo weight. Optional local `rm -rf` for a
clean working tree, but not a committed concern).
- **Rust crate versions** (tree-sitter 0.25, pyo3 0.29, thiserror 2, clap 4,
  similar 3,
rustc-hash 2) — DON'T(all latest majors; churn without benefit).
- **`EdgeBuilder` trait + 37 language impls** (`edges/base.rs`) — DON'T(legit
  polymorphism,
all registered in `get_semantic_builders()`; not a one-impl abstraction).
- **`stopwords.rs` (990), `tree_sitter_strategy.rs` `LANG_CONFIGS` table,
  `graph.rs` (1138)**
— DON'T(large **data tables** / cohesive single-domain code, not god-modules).
- **`_DEFAULT_TAU` / `DEFAULT_MAX_FILE_BYTES` duplicated across `cli.py`,
  `mcp/server.py`,
`pipeline.py`** — DON'T(INTENTIONAL: the `[tool.importlinter]` layering contract
forbids mcp importing cli/main; documented in `server.py:18-27`).
- **`compute_scored_state` / `select_with_params` / `clipboard_available`** —
  DON'T(product-dead
but load-bearing for the evaluation/sweep harness and test skip-guards; keep).
- **`eval/` research scaffolding** (`stratified_analysis`,
  `dataset_describe`,
`render_comparison`, all `harness/adapters/`/`baselines/`, `cell_metrics` numpy-free
`_percentile`) — DON'T(live research tooling; reachable via `python -m
eval <cmd>` and CI workflows, and needed to reproduce the frozen paper-v2
results (tags `paper-v2`, `paper-v2-freeze`);
`cell_metrics` being numpy-free is a deliberate hot-CI-module choice).
- **`black`→`ruff format` as an unconditional DO** — DON'T-without-gate(mature
  working tool;
see VERIFY above, not a correctness issue).

### What I didn't touch

- Correctness/resilience concerns spotted but out of leverage scope: the
  `id()`-based
caches in `graph_analytics`/`project_graph` (`_QUOTIENT_SOURCES`,
`_GRAPH_ROOTS`, bound 16 then `clear()`) are fragile under GC id-reuse — belongs
to `/review-correctness`.
- `cargo-machete` was **not installed**, so Rust dep deadness was verified by rg
  only, not
exhaustively. Re-run with `cargo install cargo-machete` for full coverage.

The repo is genuinely lean; the highest-value work was removing phantom
config that lies about what runs, not bulk deletion. The remaining upside
lives entirely in the gated VERIFY items above.
