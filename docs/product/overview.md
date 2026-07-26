# docs/product/overview.md — Product Audit (intended vs actual behavior)

Append-only cumulative log. Lens: does the product do what it is *supposed*
to do — no required behavior missing, no contract quietly broken, no
accidental behavior nobody asked for, nothing load-bearing about to break,
no genuine intent left ambiguous.

---

## Run 2026-06-20 — commit 488a9b9e

**External contract found** — README.md, CLI `--help`, CHANGELOG.md, MCP tool
descriptions, SECURITY.md, and CLAUDE.md all state user-facing promises.
Audited against those (strong intent) plus the test suite (medium intent).
The product's *functional* contract is honored well — determinism,
private-key exclusion, garbage exclusion, token pinning, exit codes are all
both promised AND tested AND verified working. The divergences below are
almost entirely **documentation-accuracy and cross-surface-consistency
drift**, not broken runtime behavior. No 🔴 (no promised-but-absent feature,
no broken runtime guarantee that users depend on).

### 🟡 P1 — "12 languages" was stale (engine supports ~37) — RESOLVED

- **Was:** README.md:53 + CLAUDE.md Technology Choices table both claimed
  AST-level parsing "across **12 languages**".
- **Actual:** the real registration (`tree_sitter_strategy.rs` LANGUAGE_CACHE,
  lines 916-1042) wires **37 tree-sitter grammar crates / 39 language names**
  (28 programming languages + 11 markup/config formats). The "12" was simply
  stale — the product does *more* than the doc claimed (undersold).
- **Done (2026-06-20):** updated both sources to "**30+ languages**" — a
  count that is comfortably true (37 crates) and resistant to upward drift, so
  it won't go stale again the way a precise number did.
  `grep '12 languages' README.md CLAUDE.md` now returns nothing.

### 🟡 P2 — `--tau` default diverged between the two shipped CLIs — RESOLVED

- **Was:** README.md:96 + Python CLI/MCP defaulted `--tau` to **0.08**, while
  the standalone Rust binary (README.md:49 "Standalone binary") defaulted to
  **0.12** (`crates/diffctx-native/src/main.rs:36` → `crates/diffctx-native/src/config/limits.rs:46`
  `DEFAULT_STOPPING_THRESHOLD`). Same repo + default flags → different
  selection between the two CLIs; the binary contradicted the documented 0.08.
- **Decision (user, 2026-06-20):** make **0.12 canonical everywhere** — it is
  the calibrated grid optimum (limits.rs:44 `(tau, cbf) = (0.12, 0.5)`).
- **Done:** raised Python defaults to 0.12 in `src/diffctx/cli.py:16`,
  `src/diffctx/mcp/server.py:22`, and `src/diffctx/_native/pipeline.py`
  (`select_with_params`, `build_diff_context`); updated help text
  (cli.py:363) and docs (README.md:96, README.md:228,
  docs/engineering/parameter-strategy.md:116). Full suite green (413 passed, 1 skipped).
- **Acceptance:** `diffctx --diff` (Python) and the Rust binary on the same
  repo with no tuning flags now share the 0.12 default; README states 0.12.

### 🟡 P3 — MCP README documented only 1 of 3 tools (truncated) — RESOLVED

- **Was:** `src/diffctx/mcp/README.md` ended at line 104 mid-entry — it
  documented `get_diff_context` (cut off after `budget_tokens`, no
  `clipboard`) and never documented `get_tree_map` or `get_file_context`.
- **Done (2026-06-20):** completed the "Available Tools" section — finished
  `get_diff_context` (added `clipboard`) and added full parameter docs for
  `get_tree_map` and `get_file_context`, matching the signatures in
  `server.py` (incl. each tool's `max_file_bytes` default of 100000).

### 🟡 P4 — `max_file_bytes` default differed across CLI / MCP — RESOLVED

- **Was:** CLI **256 KB**, MCP `get_tree_map`/`get_file_context` **100 KB**,
  Python API `map_directory` **None → 100 MB cap**. A user moving
  CLI→MCP→library got silently different truncation.
- **Done (2026-06-20):** unified MCP to the documented CLI default of
  **256 KB** via a new `_DEFAULT_MAX_FILE_BYTES = 256*1024` constant in
  `src/diffctx/mcp/server.py` (mirrors `cli.DEFAULT_MAX_FILE_BYTES` with a
  sync comment, following the same layering convention as `_DEFAULT_TAU`);
  updated the MCP README to "262144 = 256 KB". The Python API `None` default
  is left as-is — it is a deliberate, documented library semantic (caller
  opts into truncation), not part of the CLI/MCP inconsistency.

### 🟡 P5 — Version inconsistency (Rust binary mis-reported version) — RESOLVED

- **Was:** wheel ships **1.9.2** but `crates/diffctx-native/Cargo.toml` was **1.8.0**, so
  the standalone Rust binary's `--version` reported 1.8.0. The pyproject
  "keep aligned" comment itself said "= **1.7.0**" — two releases stale.
- **Done (2026-06-20):** bumped `crates/diffctx-native/Cargo.toml` to **1.9.2** (Cargo.lock
  regenerated via `cargo check`) and corrected the pyproject comment to
  "= 1.9.2". Now Cargo.toml == pyproject.toml == version.py == 1.9.2. The
  documented `dynamic = ["version"]` single-sourcing remains the eventual fix
  so they cannot drift again.

### 🟡 P6 — Undocumented env-var tuning knobs — RESOLVED (marked experimental)

- **Was:** `DIFFCTX_OP_*`, `DIFFCTX_EGO_*`, `DIFFCTX_OBJECTIVE`,
  `DIFFCTX_NO_COMMIT_SIGNAL`, `DIFFCTX_MAX_FRAGMENTS` each override a
  scoring/selection parameter and change output, with no statement of whether
  they were a supported interface.
- **Done (2026-06-20):** added a "Not a stable interface" callout to
  `docs/engineering/parameter-strategy.md` (where the `DIFFCTX_OP_*` table already lives)
  declaring all of these experimental research/calibration knobs — not a
  public API, intentionally absent from `--help`, may change between releases
  — and directing production use to the documented CLI flags. Decision: keep
  them internal rather than promote any to a supported flag.

### 🔵 P7 — `--budget` help imprecise about the default — RESOLVED

- **Done (2026-06-20):** reworded `cli.py` `--budget` help to "omit or 0 =
  auto (default), -1 = unlimited, N = fixed cap", which matches the actual
  sentinel default (no flag ⇒ auto) instead of implying the literal default
  is `0`.

### 🔵 P8 — Python API `build_diff_context` undocumented params — RESOLVED

- **Done (2026-06-20):** added `scoring_mode="ego"` and `timeout=300` to the
  README Python-API example with inline comments, so the two previously
  hidden parameters (the `timeout` foot-gun in particular) are discoverable.

### 🔵 P9 — Bare `--diff` behavior was undocumented — RESOLVED

- **Clarified:** bare `--diff` resolves to `"HEAD"`, and the engine runs
  `git diff HEAD` (git.rs:147-151) = **working tree vs HEAD = uncommitted
  changes**. So it is empty only when the tree is clean (the
  `test_bare_diff_defaults_to_head` fixture) — intuitive, not a bug.
- **Done (2026-06-20):** added a README usage example
  `diffctx . --diff   # uncommitted changes (working tree vs HEAD)` so the
  behavior is shown rather than implied.

### Do-not-change invariants (named so a later cleanup doesn't break them)

- Output node schema: `type` ∈ {`file`,`directory`}; diff fragments carry
  `role` only when overlapping a hunk; changed fragments emitted first. Locked
  by `tests/test_diffctx_invariants.py`. treemapper (`diffctx>=1.7,<2.0`) +
  MCP clients parse this — a rename/reshape is a breaking change.
- Token encoding pinned to `o200k_base`
  (`test_tiktoken_o200k_base_encoding_is_pinned`); changing it breaks paper
  reproducibility.
- `panic = "abort"` in the release profile (FFI safety across PyO3 boundary).

---

## OPPORTUNITY (not a requirement) — ranked, human is the gate

> Net-new suggestions, never installed as existing requirements. Anchored to
> friction observed above; abstain by default. The planned secret-value
> content-scan redaction (CHANGELOG, "separate planned content-scan feature")
> is **dropped** here — already tracked.

1. **OPPORTUNITY (not a requirement):** `diffctx --list-languages` printing
   the actually compiled grammar set. Anchors to P1 — the "12" drifted
   precisely because the supported set lives only in Cargo.toml and nobody
   re-counts it; a command that emits the live list makes the docs
   self-checking and gives users a real answer.
2. **OPPORTUNITY (not a requirement):** a parity self-test asserting the
   Python CLI and the standalone Rust binary produce identical default-flag
   selection. Anchors to P2/P5 — the tau and version splits both came from two
   surfaces drifting with nothing pinning them together.
