# LEVERAGE — diffctx

Cumulative leverage-audit log (simplify · DRY/reuse · modernize · restructure ·
clean). Append-only, changelog-style. Each section = one `/review-leverage` run.

## 2026-07-19 — 34872c1b

Scope: full repo (Python `src/`, Rust crate `diffctx/src/`, `benchmarks/`,
`scripts/`, `tests/`, tooling/CI). Maturin/pyo3: heavy logic lives in Rust
(`diffctx._diffctx`), Python is a thin CLI/MCP/tree wrapper. Topology: 6
parallel scouts (Python pkg, Rust crate, benchmarks/scripts, tooling/CI,
detectors, tests) → adversarial verification of DO findings → this verdict.
First run; no prior section to reconcile against.

Headline: the codebase is unusually clean. Biggest wins are **phantom config**
(config referencing modules/tools that no longer exist) and a handful of
micro-deletions. Most "suspicious" weight (test-repos corpora, caches, orphaned
`.pyc`) is already gitignored and carries **zero repo weight**. Detector caveat:
`cargo-machete` was unavailable, so Rust deps were audited manually (rg), not
exhaustively.

---

### DO — act now (in-scope, high-confidence)

## [🟡] `[tool.mutmut]` is a fully orphaned phantom gate

Category: Delete / phantom gate · Location: `pyproject.toml:303-309` · Evidence:
`do_not_mutate = [ "src/diffctx/diffctx/parsers/__init__.py",
"src/diffctx/diffctx/stopwords.py" ]` — both paths are **gone** (`ls` → "No such
file or directory"; only a stale `stopwords.cpython-312.pyc` orphan remains).
`mutmut` is **not** a declared dep (absent from `optional-dependencies.dev`) and
appears in **no** workflow or pre-commit hook (`rg mutmut .github/
.pre-commit-config.yaml` → 0). · Problem: the mutation-testing gate advertised
by CLAUDE.md/memory does not run and points at deleted files — false confidence,
worst-class dead weight. · Recommendation: remove the whole `[tool.mutmut]`
block (or re-arm: add `mutmut` to dev deps + a workflow + fix the paths, if
mutation testing is still wanted). · Expected benefit: −7 lines, removes a lie.
· Effort: Easy · Confidence: High · Verification: `rg mutmut pyproject.toml` → 0
(or a green mutmut CI job).

## [🟡] Phantom mypy overrides for deleted internal modules

Category: Delete / phantom config · Location: `pyproject.toml:233-235,245` ·
Evidence: overrides list `"diffctx.diffctx.edges"`, `"diffctx.diffctx.edges.*"`,
`"diffctx.diffctx.embeddings"` and `"diffctx.diffctx.universe"` — none exist
under `src/diffctx/diffctx/` (only
`__init__/graph_analytics/graph_export/pipeline/project_graph.py` are tracked;
`embeddings`/`universe` survive only as orphaned `.pyc`). · Problem: stale
config masquerading as active type policy; a mypy override on a non-existent
module is a silent no-op. · Recommendation: delete the 4 dead module entries. ·
Expected benefit: −4 lines, honest config. · Effort: Easy · Confidence: High ·
Verification: `mypy src` still clean; every remaining override module resolves.

## [🟡] `_normalize_budget` duplicated inline + dead keep-alive line

Category: DRY / cut · Location: `src/diffctx/diffctx/pipeline.py:102-110` ·
Evidence: the `if/elif/elif/else` chain assigning `effective_budget` is
behaviourally identical to `_normalize_budget()` (line 17) — the `elif
budget_tokens == 0: effective_budget = 0` case collapses into the helper's else
(`_normalize_budget(0)` returns `0`). Line 110 is a literal no-op: `_ =
_normalize_budget  # keep helper available; mirrors the same semantics`. ·
Problem: two copies of the same 4-way policy plus a dead statement that only
exists because the helper was otherwise unused in this function. ·
Recommendation: replace 102-110 with `effective_budget: int | None =
_normalize_budget(budget_tokens)`. · Expected benefit: −9 LOC, single source of
budget semantics. · Effort: Easy · Confidence: High · Verification: `pytest
tests/test_diffctx_invariants.py` + an E-class equivalence run
(behaviour-preserving).

## [🟡] Rust: unused `smallvec` dep + duplicate `serde_yaml` dev-dep

Category: Delete / dep hygiene · Location: `diffctx/Cargo.toml:121,129` ·
Evidence: `rg -in smallvec diffctx/src` → 0 hits (only the Cargo.toml line);
`serde_yaml = "0.9"` appears in both `[dependencies]:111` and
`[dev-dependencies]:129` (same version, no extra features — dev targets already
inherit normal deps). · Problem: a declared-but-unused crate and a redundant
dev-dep line. · Recommendation: delete both lines. · Expected benefit: −2 lines,
−1 crate from the graph. · Effort: Easy · Confidence: High · Verification: `cd
diffctx && cargo build --all-features` clean.

## [🔵] Dead `numpy` optional-dependency extra + paired mypy override

Category: Delete · Location: `pyproject.toml:80-82,225` · Evidence:
`optional-dependencies.diffctx = [ "numpy>=1.24,<3.0" ]` but `rg 'import numpy'
src/` → 0, and `diffctx[diffctx]` is referenced by nothing (not `full`, not
scripts). The `numpy` mypy override (line 225) is paired dead weight. · Problem:
published extra that installs a heavy dep no `src/` code path uses. ·
Recommendation: remove the `diffctx` extra + the numpy override. · Expected
benefit: −4 lines, smaller install surface. · Effort: Easy · Confidence: High ·
Verification: `rg -n numpy src/` → 0 (re-add only if embeddings return).

## [🔵] Dead exception class `DiffContextTimeoutError`

Category: Delete · Location: `src/diffctx/diffctx/pipeline.py:7-8` · Evidence:
`class DiffContextTimeoutError(Exception):` — `rg DiffContextTimeoutError src
tests benchmarks` matches only this definition; not in `__all__` (nested
`__init__` exports `GitError, build_diff_context, compute_scored_state,
select_with_params`), never raised, never imported. The Rust backend enforces
`timeout` and surfaces its own error. · Recommendation: delete the class. ·
Expected benefit: −2 LOC. · Effort: Easy · Confidence: High · Verification:
`pytest tests/` green; `rg DiffContextTimeoutError` → 0.

## [🔵] Micro-cuts (dead branches / aliases / defensive no-ops)

Category: Cut · Confidence: High · Effort: Easy · verdict DO (each
independently):

- `src/diffctx/tokens.py:46-49` — `print_token_summary` branches on
  `result.is_exact`,
but `count_tokens` hard-codes `is_exact=True` (line 25, the only constructor),
so the `else` is unreachable. Drop the `else` (−2 LOC). Keep the dataclass field
(asserted in tests) — field removal is VERIFY, not DO.
- `src/diffctx/writer.py:32-33` — `_YAML_STRING_ESCAPE_MAP =
  _YAML_BASE_ESCAPE_MAP` is a
pure alias (three names for two maps). Name the base map directly, drop the
alias (−1 LOC).
- `src/diffctx/main.py:167` — `getattr(args, "scoring", "ego")` on a
  `ParsedArgs` dataclass
field that always exists (`cli.py:252 scoring: str = "ego"`). Use `args.scoring`
(readability).

---

### VERIFY — do iff the named gate passes

## [🟡] Rust: `once_cell::sync::Lazy` → `std::sync::LazyLock`

Category: Modernize · Location: `diffctx/Cargo.toml` + ~74 `.rs` files (`use
once_cell::sync::Lazy;`) · Evidence: crate is edition 2024 / `rust-version =
"1.85"`; `LazyLock` is stable since 1.80; no `unsync`/`OnceCell` uses exist (all
uses are `sync::Lazy`). · Problem: an external crate for what std now provides.
· Recommendation: mechanical `Lazy`→`LazyLock` rename, drop the `once_cell` dep.
· Expected benefit: −1 crate, ~148 import/type lines churned (net LOC ≈ flat). ·
Effort: Medium · Confidence: High it's a drop-in (both `Deref`-based). · verdict
**VERIFY(gate: `cd diffctx && cargo build --all-features` clean +
equivalence_gate double-run — E-class change per CLAUDE.md, must not alter
selection output)**.

## [🟡] Vestigial `tree-sitter` Python optional-deps group (17 wheels)

Category: Delete / dep surface · Location: `pyproject.toml:91-109` (+ `full` and
`dev` self-refs to `diffctx[tree-sitter]`) · Evidence: `rg 'import
tree_sitter|from tree_sitter' src tests benchmarks scripts` → 0 — parsing is
fully migrated to the Rust `tree-sitter-*` crates (`diffctx/Cargo.toml`). The 17
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

Category: DRY · Location: `diffctx/src/edges/semantic/*.rs` (e.g.
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

## [🔵] `benchmarks/backfill_checkpoints.py` — one-off legacy migration

Category: Delete · Location: `benchmarks/backfill_checkpoints.py:1` · Evidence:
docstring "Enrich existing checkpoint.jsonl files … Old sweep artifacts predate
the evaluator stamping n_gold, gold_to_changed_ratio …"; 142 LOC, zero refs (no
CI, no `__main__` dispatch, no RESUME.md, no import). · Problem: backfill for
artifacts predating current field-stamping. · Recommendation: delete once
obsolete. · Expected benefit: −142 LOC. · Effort: Easy · Confidence: Medium ·
verdict **VERIFY(gate: confirm the active `sweep_v2_local` evaluator stamps
every field `cell_metrics.py` reads, so no legacy checkpoint needs backfilling —
then delete)**. NOTE: active paper-v2 sweep is paused mid-run; do not delete
while any resumable checkpoint could still need enrichment.

## [🔵] Rust: cargo-cult no-op `_used` fn; `serde_yaml` unmaintained

Category: Cut / modernize · Confidence: Medium · verdict VERIFY (each):

- `diffctx/src/pybridge.rs:781-783` — `#[allow(dead_code)] fn _used(_: PathBuf)
  {}` with
comment "silence unused warnings for pyclass getter fields". A no-op fn cannot
suppress field warnings; the build is warning-clean without it. Delete (−3 LOC).
VERIFY(gate: `cargo build --all-features --features python` still warning-clean
after removal).
- `diffctx/Cargo.toml:110` — `serde_yaml 0.9` is archived/unmaintained (dtolnay
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

- **`test-repos/` corpora, `diffctx/target/`, `.mypy_cache/`, `.ruff_cache/`,
`.hypothesis/`, `.pytest_cache/`, `.playwright-mcp/`,
`.import_linter_cache/treemapper.meta.json`, and the ~30 orphaned `.pyc` in
`src/diffctx/diffctx/__pycache__/`** — DON'T(all verified **gitignored / 0
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
- **`benchmarks/` research scaffolding** (`stratified_analysis`,
  `dataset_describe`,
`render_comparison`, all `adapters/`/`baselines/`, `cell_metrics` numpy-free
`_percentile`) — DON'T(active paper-v2 cycle; reachable via `python -m
benchmarks <cmd>`, CI workflows, or the pending paper-table regen;
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

---

### Total Estimated Savings

**DO (immediate, verified):**

- Config phantom/dead: mutmut block (−7), mypy phantom modules (−4), numpy
  extra+override
(−4) = **−15 config lines**, removes 2 phantom gates.
- Rust deps: `smallvec` + duplicate `serde_yaml` dev-dep = **−2 lines, −1
  crate**.
- Code micro-cuts: pipeline budget dedup (−9), `DiffContextTimeoutError` (−2),
  tokens dead
branch (−2), writer alias (−1), main.py getattr (0) = **≈ −14 LOC**.
- **DO subtotal: ≈ −31 lines + −1 Rust crate + 2 phantom gates removed.**

**VERIFY (gated, larger upside):**

- Tests parametrize/dedup: **≈ −290 LOC** (gate: pytest green).
- `benchmarks/backfill_checkpoints.py`: **−142 LOC** (gate: evaluator stamps all
  fields; not while sweep paused).
- Rust `is_*_file` DRY: **−60…80 LOC** (gate: E-class equivalence).
- `once_cell`→`LazyLock`: **−1 crate** (gate: cargo build + equivalence).
- `tree-sitter` Python extra: **−19 lines**, big install-surface trim (gate: no
  downstream consumer).
- `mcp/formatting.py` inline: **−9 LOC / −1 file**; `pybridge.rs _used`: −3 LOC.
- **VERIFY subtotal: ≈ −520 LOC + −2 crates + smaller published-install
  surface**, all gated.

**Grand total if all DO + passing VERIFY applied: ≈ −550 LOC, −3 Rust crates, −2
phantom gates, 1 dead published extra, 1 formatter removed.** The repo is
genuinely lean; the highest-value work is removing phantom config that lies
about what runs, not bulk deletion.
