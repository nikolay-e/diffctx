# docs/product/overview.md — Product Audit (intended vs actual behavior)

Append-only cumulative log. Lens: does the product do what it is *supposed*
to do — no required behavior missing, no contract quietly broken, no
accidental behavior nobody asked for, nothing load-bearing about to break,
no genuine intent left ambiguous.

---

## Run 2026-06-20 — commit 488a9b9e

All nine findings from this run (P1–P9: stale language count, `--tau` default
divergence, truncated MCP README, `max_file_bytes` inconsistency, Rust binary
version drift, undocumented env-var knobs, `--budget` help wording, hidden
Python-API params, bare `--diff` semantics) were **resolved in-run**. The
per-finding detail is dropped — the line numbers, versions, and defaults it
cited have since moved on. Only the durable invariants and the still-open
opportunity below survive.

### Do-not-change invariants (named so a later cleanup doesn't break them)

- Output node schema: `type` ∈ {`file`,`directory`}; diff fragments carry
  `role` only when overlapping a hunk; changed fragments emitted first. Locked
  by `tests/test_diffctx_invariants.py`. MCP clients parse this — a
  rename/reshape is a breaking change.
- Token encoding pinned to `o200k_base`
  (`test_tiktoken_o200k_base_encoding_is_pinned`); changing it breaks paper
  reproducibility.
- `panic = "abort"` in the release profile (FFI safety across PyO3 boundary).

---

## OPPORTUNITY (not a requirement) — ranked, human is the gate

> Net-new suggestions, never installed as existing requirements. Anchored to
> friction observed above; abstain by default.

1. **OPPORTUNITY (not a requirement):** `diffctx --list-languages` printing
   the actually compiled grammar set. Anchors to P1 — the "12" drifted
   precisely because the supported set lives only in Cargo.toml and nobody
   re-counts it; a command that emits the live list makes the docs
   self-checking and gives users a real answer. Still absent as of
   2026-07-26.

(The parity-self-test opportunity from this run is done:
`crates/diffctx-native/tests/native_cli.rs` contract-tests Python/binary
parity.)
