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
