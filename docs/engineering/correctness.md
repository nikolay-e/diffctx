# CORRECTNESS — /review-correctness findings log

## 2026-06-14 · 71bf302 · diffctx Rust core (10.2k LOC, all of src/)

### TL;DR

No 🔴 critical correctness bugs. `cargo clippy --all-targets` is clean
(pedantic-only), so existence/type/signature errors are ruled out by the
compiler. The hard algorithms — forward-push PPR, CELF lazy-greedy
selection, unified=0 hunk parsing — were read line by line and verified
correct. Real defects are three 🟡 edge/efficiency issues and a handful of
🔵 notes.

### Top Issues (verified ✅)

1. ✅ 🟡 `postpass.rs:210,248` — `_batch_reader: Option<&mut CatFileBatch>`
   is unused; the missing-changed-file fallback calls
   `create_whole_file_fragment(path, root_dir, preferred_revs, None)` —
   always `None`. Caller (`pipeline.rs:164`) builds a `CatFileBatch` and
   passes `batch_reader.as_mut()`, but it is never used: every rescued
   changed file spawns a fresh `git show` subprocess instead of reusing the
   batch. Correct output, wasted process spawns, and a parameter name that
   lies about plumbing that doesn't happen.
2. ✅ 🟡 `git.rs:379` vs `git.rs:396` / `candidate_files.rs:79-84` — path
   normalization is inconsistent. `get_changed_files` returns
   `repo_root.join(p)` uncanonicalized, while `get_deleted_files` (`:396`)
   and discovered files (`normalize_path`) DO canonicalize. When a tracked
   path traverses a symlinked directory component, the same physical file
   gets two distinct `PathBuf` → `FragmentId` keys, so `seen_frag_ids`
   (`pipeline.rs:163`) fails to dedup → duplicate fragments for one file.
   Edge case (symlinked tracked dir), but a genuine normalization mismatch.
3. ✅ 🟡 `signatures.rs:75` — `find_signature_end` returns at the first line
   where `ob - cb > 0` (any unmatched `{`). `count_brackets_outside_strings`
   strips string literals but not braces inside parameter defaults or
   annotations, so a Python `def f(x={}):` (or a multi-line header with a
   dict/type-brace default) terminates the signature mid-parameter-list,
   emitting a wrong `sig_end_line`. Supplementary signature variant only.

### 🔵 Notes

- `interval.rs:52` — `overlaps` uses strict `end > start`; with inclusive
  `[start,end]` ranges (`types.rs:230` `end-start+1`) two fragments sharing
  a boundary line both pass and that shared line can render twice. The
  comment frames it as "adjacent, not overlapping" — a deliberate,
  documented tradeoff (strict `>` avoids dropping the next fragment in
  compact languages). Terminologically loose, but intentional; not a bug.
- `tokenizer.rs:43` — degraded fallback `((text.len() as u32)/4).max(1)`
  returns 1 for the empty string (vs 0 on the normal path) and over-counts
  multibyte (Cyrillic/emoji) text ~2-4×. Only on encoder-init failure.
- `filtering.rs:147` — `*count >= 1` is always true (counts start at 1);
  dead predicate, the real gate is `deps.len() >= hub_reverse_threshold`.
  Misleading, harmless.
- `ppr.rs` truncated runs are silently sum-renormalized; documented in the
  comment + a `tracing::warn!`, but downstream selection consumes the biased
  scores with no programmatic guard. Reachable only on graphs hitting
  `max_pushes_cap` (~>20k fragments). Documented.

### False Positives (looked bad, verified fine)

- CELF lazy-greedy stale-gain commit (`select.rs:285-315`): correct — pops
  top, if `version < cv` recomputes against live state and re-pushes,
  commits only when versions match. Textbook CELF.
- Render escaping: `render.rs` delegates to `serde_yaml`/`serde_json` — no
  hand-rolled YAML literal-block/quoting bugs.
- Hunk parsing `@@ -a,b +c,d @@` with omitted count: defaults to 1 per git
  spec (`git.rs:302,307`); deletions (`new_len==0`) handled
  (`types.rs:246`).
- `fragmentation.rs:147` `lines.len() - max_lines` underflow: unreachable
  (`create_snippet` joins exactly `line_count` lines; early-return
  guarantees `len > max_lines`).
- "Changed-file fragments render with absolute paths": refuted — both
  changed and discovered paths retain the `repo_root` prefix, so
  `strip_prefix` succeeds.

### Resolution (same run — all addressed)

- 🟡 #1 `postpass.rs` — `_batch_reader` wired through via
  `batch_reader.as_deref_mut()`; rescued files now reuse the `CatFileBatch`
  instead of spawning a `git show` each.
- 🟡 #2 `git.rs` — `get_changed_files` and `get_untracked_files` now
  canonicalize (`repo_root.join(p).canonicalize()`), matching
  `get_deleted_files`/`normalize_path`.
- 🟡 #3 `signatures.rs` — body-brace detection gated on `paren_depth <= 0`,
  so braces in parameter defaults/annotations no longer truncate the
  signature.
- 🔵 `tokenizer.rs` — degraded fallback returns 0 for the empty string
  (matches normal path).
- 🔵 `filtering.rs` — dead `*count >= 1` predicate removed.
- 🔵 `interval.rs` — comment corrected to state the deliberate
  one-line-overlap tradeoff (behavior unchanged — strict `>` is
  intentional).
- 🔵 `ppr.rs` truncation renormalization — left as-is (by design, already
  documented + warns).

### Verification

- `cargo build` + `cargo clippy --lib` clean; `pytest` 409 passed /
  1 skipped.
- `cargo test --test yaml_cases` is a quality-benchmark eval, not a
  pass/fail gate: ~2258/2725 pass on clean HEAD and on the
  pre-dependency-bump commit `98438386` alike, so the ~465 "failures" are
  the corpus's standing baseline (hard cases below the per-case 10% score
  threshold), NOT a regression. The suite is mildly flaky (~5
  threshold-boundary cases flip per run). My changes hold the aggregate
  within that noise band — no real regression.

### Scouts/synthesis

4/0 (module-scale, folded into verdict per pyramid).

## 2026-06-14 17:21 · f44b2c74 · full engine (Rust: scoring/ppr/select/git/edges/render/parsers)

### TL;DR

Root-caused and FIXED the "mild flakiness" the previous entry accepted as
noise: the diff-context pipeline was genuinely **nondeterministic across
processes** (same input → different selection), violating the advertised
"Deterministic, side-effect-free" guarantee. Also fixed a real
cross-language inconsistency in Python import edges. After fixes,
`cargo test --test yaml_cases` is **bit-for-bit deterministic** across runs
(two full runs → identical failure sets); aggregate unchanged
(2258/2725), so determinism gained with zero ranking regression.

### Top Issues (fixed)

1. 🔴 `graph.rs:233` (`ego_graph`) — `*scores.entry(idx).or_insert(0.0) +=
   contribution` accumulated in **seed-iteration order**, where seeds is an
   `FxHashSet<FragmentId>` keyed by absolute paths containing the per-run
   random temp dir. Non-associative float `+=` in a per-process-varying
   order → scores differing by ULPs → near-tie fragments flip in selection
   → the same test case passes or fails at random across runs (persisted
   even single-threaded). FIX: `valid_seed_idxs.sort_unstable()` before the
   accumulation (seed indices are run-stable values via `freeze()`'s
   `nodes.sort()`).
2. 🔴 `filtering.rs:289` (`cap_context_fragments`) — within-file cap used an
   **unstable** sort (`partial_cmp`, no tie-break) before `take(N)`, and the
   candidate vector was emitted in `FxHashMap`(path)-iteration order. With
   tied rel-scores, `take(N)` dropped different fragments per run. FIX:
   `total_cmp().then_with(|| a.id.cmp(&b.id))` + final `result.sort_by(id)`
   to canonicalize candidate order.
3. 🟡 `select.rs:73` (`HeapEntry::cmp`) — greedy selection heap tie-broke on
   density only; equal-density entries popped in insertion order. Added a
   stable `frag_id` secondary key (defense-in-depth; the two leaks above
   were the dominant cause).
4. 🟡 `edges/semantic/python.rs:253` — Python **module-import** edges were
   inserted forward-only (importer→module), bypassing `base::add_edge`,
   while Python's own symbol-ref edges (same file, ~237) and every other
   language's import edges (go/js/rust) are bidirectional via `add_edge`
   (forward + `reverse_factor` reverse). The graph is built directed with no
   symmetrization (`graph.rs:383`), so a changed imported module never
   propagated relevance back to its importers. FIX: route through
   `base::add_edge(..., import_weight, reverse_factor)`.

### Systemic Pattern

Per-process nondeterminism from iterating `FxHashMap`/`FxHashSet` **keyed by
fragment paths/ids** (which embed the random temp dir in tests, and absolute
paths in prod) and letting that order drive (a) non-associative float
accumulation and (b) tie-affected truncation/selection. `FxHashMap` is only
deterministic for *identical key sets in identical processes*; any output
ordering derived from it must be re-sorted by a stable key. Root cause is
not parallelism (reproduced with `RAYON_NUM_THREADS=1`).

### False Positives (verified, no change)

- 🔵 `scoring.rs:215` / `discovery.rs:240` BM25 IDF uses `.ln_1p()`
  (= `log(1 + (N−df+0.5)/(df+0.5))`). NOT a bug — this is the
  Lucene/Elasticsearch default BM25 IDF, deliberately chosen for
  non-negativity; downstream code gates on `score > 0.0` and feeds PPR seed
  weights, so plain `.ln()` (which can go negative when df > N/2) would
  introduce a bug. Confirmed correct.
- 🔵 Path-canonicalization "mismatch" in `git.rs`/`core.rs` — false alarm.
  Both the diff-hunk key and the fragment key use the same non-canonical
  `repo_root.join(rel)` form; `parse_path_line` canonicalizes only for the
  containment guard. No lookup mismatch.
- 🔵 PPR (`ppr.rs`) — verified correct: teleport `1−alpha`, sum-to-1
  personalization, dangling handling, and a closed-form star-graph test to
  <1e-3. Internally deterministic (`freeze()` sorts nodes).

### Verification

- `cargo build`, `cargo clippy --lib` clean (only a pre-existing
  float-`!=` warning in `edges/mod.rs`); `cargo test --lib` 57/57.
- Determinism proof: two consecutive `cargo test --test yaml_cases` runs →
  **identical** failure sets (was ~463–466 fluctuating; now stable at 467).
  gap_008 (the Python-import eval case) went from ~50/50 flaky to 10/10
  stable. Aggregate pass count within prior noise band → no regression.
- The 467 standing eval-threshold failures are a ranking-quality tuning
  matter (out of scope here) — but now reproducible, so they can be worked
  on reliably.
- Cross-process CLI determinism confirmed for BOTH modes (real
  `python -m diffctx` runs, byte-identical output): tree-mapping
  (`diffctx <dir>`, structure-only and full-content) over 8 runs, and
  diff-context (`diffctx . --diff HEAD~1`) over 5 runs.

### Scouts/synthesis

5 scouts / 3 synthesis-verifiers (1 dedicated determinism root-cause hunt).

---

## Run 2026-06-20 (commit f774165) — 8→4→2→1 pyramid

Deterministic layer already green (pytest 413, `cargo test --lib` 57, clippy,
mypy, full `yaml_cases`). 8 scouts → 4 adversarial verifiers → 2 final
verifiers → orchestrator. Most scout "criticals" were REFUTED on verification;
the pyramid's value showed twice (the Haskell "typo" and the role-stripping
claim both flipped to refuted only at R2/R3).

### Confirmed + fixed

- 🟡 **Citation builder could emit self-edges.** `edges/document/mod.rs`
  CitationEdgeBuilder pushes a fragment id once per regex match, so a fragment
  citing the same key twice could become a `(hub, hub)` edge; `graph.rs
  add_edge` had no `src == dst` guard (other builders like AnchorLink/
  Containment guard locally). Fix: central guard `if src == dst { return; }`
  in `add_edge` (graph.rs:116). Verified no algorithm relies on self-edges.
- 🟡 **Test→source import edges matched imports in comments/strings.**
  `edges/structural/testing.rs:14` `IMPORT_RE` lacked a line anchor, unlike the
  Python semantic builder. Fix: `(?m)^\s*` prefix — still matches indented
  imports, drops false matches inside comments/strings/docstrings.
- 🟡 **MCP `get_file_context` over-reported copied file count.**
  `mcp/server.py` `_build_file_content_report` returned `len(matched)` while
  skipping size-exceeding files, so "Copied N files" overstated. Fix: return
  `included_count` (only files whose content was actually emitted).
- 🟡 **Stale normative parameter doc.** `docs/engineering/parameter-strategy.md`
  documented `EGO.per_hop_decay (γ) = 1.0`, but the code default has been `0.5`
  since commit 6c28522b ("paper-aligned EGO scoring"); ego is the default
  scoring mode, and the env clamp `(EPS, 1-EPS)` makes 1.0 unreachable anyway.
  Doc is the wrong side — updated to 0.5 (code is correct).
- 🔵 **`--tau` had no upper bound and misleading help.** `cli.py _validate_tau`
  allows tau>1, which silently keeps only changed code (`select.rs`:
  `best_density < tau * peak_density` trips immediately). Clarified help text
  ("typical 0-1; >1 keeps only changed code"); no behavior change.

### Refuted (scout false positives — recorded so they aren't re-raised)

- **NOT a bug: `node_end_line` `+1`** (`tree_sitter_strategy.rs`). tree-sitter
  `end_position().row` is 0-indexed inclusive of the last content line; `+1`
  yields the correct 1-indexed inclusive display end AND the correct exclusive
  slice bound (`mod.rs create_snippet` uses `lines[start-1..end]`). Tests agree.
- **NOT a bug: `role` stripped for consumers.** `pybridge.rs to_serializable`
  / `PyFragment` do drop `role`, but the real path
  (`#[pyfunction] build_diff_context` → dict) sets `role` directly from
  `DiffContextOutput`; MCP/CLI use that path; the role invariant test passes.
  `DiffContextResult.to_yaml/to_json` is a secondary path with no consumer
  (🔵 latent only — left as-is).
- **NOT a typo: Haskell `type_synomym`** (`tree_sitter_strategy.rs`). Matches
  the actual tree-sitter-haskell 0.23.1 grammar node name (misspelled
  upstream). "Fixing" to `type_synonym` would BREAK Haskell extraction.
  Verified against the crate's `node-types.json`. All other definition_types
  from commit 4d39f58b also match their grammars.
- **NOT a bug:** lexical clamp `.max()` of per-language minimums (intentional
  "stricter language wins"); sibling 20-file cap (configured perf cap);
  hub-suppression `>` vs `>=` (`>` correct); `extract_identifiers` includes
  keywords (two-phase design, filtered downstream); deletion `new_start` `.max(1)`
  normalization (harmless); MCP double-`resolve()` (redundant but secure — no
  traversal hole found).

### Verification

- `cargo test --lib` 57/57; full `yaml_cases` **2260 passed / 465 failed —
  identical to pre-fix baseline** (self-edge guard + import anchor fix edge
  cases the synthetic suite doesn't exercise; no regression). pytest 413 passed,
  1 skipped; test_mcp + test_cli 27 passed.

### Scouts/synthesis

8 scouts / 4 adversarial verifiers / 2 final verifiers / 1 orchestrator.
