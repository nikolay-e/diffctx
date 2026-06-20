# PRODUCT.md — Product Audit (intended vs actual behavior)

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
  **0.12** (`diffctx/src/main.rs:36` → `diffctx/src/config/limits.rs:46`
  `DEFAULT_STOPPING_THRESHOLD`). Same repo + default flags → different
  selection between the two CLIs; the binary contradicted the documented 0.08.
- **Decision (user, 2026-06-20):** make **0.12 canonical everywhere** — it is
  the calibrated grid optimum (limits.rs:44 `(tau, cbf) = (0.12, 0.5)`).
- **Done:** raised Python defaults to 0.12 in `src/diffctx/cli.py:16`,
  `src/diffctx/mcp/server.py:22`, and `src/diffctx/diffctx/pipeline.py`
  (`select_with_params`, `build_diff_context`); updated help text
  (cli.py:363) and docs (README.md:96, README.md:228,
  docs/parameter-strategy.md:116). Full suite green (413 passed, 1 skipped).
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

### 🟡 P4 — `max_file_bytes` default differs across CLI / MCP / Python API

- **Actual three defaults:** CLI **256 KB** (`src/diffctx/cli.py:14`
  `DEFAULT_MAX_FILE_BYTES = 256*1024`); MCP `get_tree_map` &
  `get_file_context` **100 KB** (`src/diffctx/mcp/server.py:102,209`); Python
  API `map_directory` **None → 100 MB safe cap**
  (`src/diffctx/__init__.py:50`, `tree.py` `MAX_SAFE_FILE_SIZE`).
- **Intent:** README documents the CLI 256 KB (README:281) and the API `None`
  (README:205); the MCP 100 KB is undocumented (see P3).
- **Kind:** accidental/ambiguous behavior — a user moving CLI→MCP→library
  gets silently different truncation for the same file.
- **Acceptance:** the three surfaces share one default, OR each surface's
  default is documented with its rationale and the difference is intentional.

### 🟡 P5 — Version inconsistency (Rust binary mis-reported version) — RESOLVED

- **Was:** wheel ships **1.9.2** but `diffctx/Cargo.toml` was **1.8.0**, so
  the standalone Rust binary's `--version` reported 1.8.0. The pyproject
  "keep aligned" comment itself said "= **1.7.0**" — two releases stale.
- **Done (2026-06-20):** bumped `diffctx/Cargo.toml` to **1.9.2** (Cargo.lock
  regenerated via `cargo check`) and corrected the pyproject comment to
  "= 1.9.2". Now Cargo.toml == pyproject.toml == version.py == 1.9.2. The
  documented `dynamic = ["version"]` single-sourcing remains the eventual fix
  so they cannot drift again.

### 🟡 P6 — Undocumented env-var tuning knobs silently change selection

- **Actual:** `diffctx/src/config/env_overrides.rs` + callers read
  `DIFFCTX_OP_*`, `DIFFCTX_EGO_PER_HOP_DECAY`, `DIFFCTX_OBJECTIVE`,
  `DIFFCTX_NO_COMMIT_SIGNAL`, `DIFFCTX_MAX_FRAGMENTS`, etc. — each overrides a
  scoring/selection parameter and changes output. None appear in README /
  `--help` / SECURITY.md. (Tests *do* exercise some, e.g. `DIFFCTX_OBJECTIVE`,
  `DIFFCTX_OP_SELECTION_CORE_BUDGET_FRACTION` — so they're intended internal
  knobs, just unpublished.)
- **Kind:** accidental public surface. A human decides: keep them internal
  (note in CLAUDE.md as research-only) or document a supported subset.
- **Acceptance:** docs state which env vars are supported-vs-internal;
  unsupported ones are clearly marked experimental.

### 🔵 P7 — `--budget` help says "0=auto (default)" but literal default is unset

- `src/diffctx/cli.py` `--budget` help: "0=auto (default)". Argparse default
  is actually a sentinel (`_UNSET` → `None`), which routes to the same auto
  path as `0`. Functionally correct, literally imprecise.

### 🔵 P8 — Python API `build_diff_context` exposes undocumented params

- `pipeline.py` `build_diff_context(..., scoring_mode="ego", timeout=...)` —
  neither appears in the README Python-API example (README:220-231), which
  only shows `diff_range/budget_tokens/alpha/tau/full`. `timeout` is a quiet
  foot-gun. A small doc addition would close it.

### 🔵 P9 — Bare `--diff` → `HEAD` yields empty context for uncommitted work

- `src/diffctx/cli.py` resolves bare `--diff` to `"HEAD"` (documented). For a
  user with *uncommitted* changes, `--diff` (= `HEAD..HEAD`) produces "no
  semantic context" (tested: `test_bare_diff_defaults_to_head`). Intended per
  the test, but README examples never show the empty-on-dirty-tree footgun.
  Is this the intended default, or should bare `--diff` mean working tree vs
  HEAD?

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
