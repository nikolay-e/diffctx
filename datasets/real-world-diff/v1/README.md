# diffctx Real-World Diff Benchmark

A SWE-bench-style benchmark for the **quality** (not just the liveness) of the
context `diffctx --diff` extracts. Where the synthetic `yaml_cases` suite tests
selection on hand-built repos, this benchmark tests it on **109 real upstream
commits** across three large OSS repos (react-native 35, gitpod 37, sentry 37 —
the curated set in `test-repos/TOANALYZE.md`), each hand-labeled with what a
correct tool *should* and *should-not* surface.

## Why this exists

QA had established that diffctx does not crash. This benchmark asks the next
question — **is the extracted context any good?** — measured as
comprehension-per-token: does the output let a reader understand the change
without dragging in the whole tree, and without burning the budget on noise?

## How it was built

1. For each of the 109 commits, generate the ground-truth `git diff` plus
   diffctx's own output (`-f md` and `-f yaml`, current shipped `1.10.2`), under
   a 30s wall-clock cap. Record status (`ok` / `over_dump` / `hang` / `empty`),
   token counts, fragment counts, and the set of files diffctx rendered.
2. Ten independent **Sonnet-5 reviewers** each read a shard (~11 commits): the
   real diff (what changed) and diffctx's extracted context (what it pulled in),
   then labeled per commit:
   - `gold_include` — files genuinely required to understand the change.
   - `gold_exclude` — files diffctx surfaced that a good tool would not.
   - relevance/completeness/token-efficiency/format judgements + a log of concerns.
3. diffctx's **actual** selection is then scored deterministically against those
   gold labels (see Metric).

## Files

| File | What |
|---|---|
| `diffctx_realworld_bench.jsonl` | one scored record per commit (paths + metrics only, no source) |
| `gold_labels.json` | the 10 reviewers' full per-commit labels + concern logs |
| `quality_report.md` | the synthesized engineering quality report (named examples) |
| `summary.json` | headline aggregate |
| `commits.tsv` | the 109 `repo, idx, sha, files, description` rows |

## Metric

Per commit, treating diffctx's rendered file set as the "selected" set:

```text
precision      = |selected ∩ gold_include| / |selected|
recall         = |selected ∩ gold_include| / |gold_include|
forbidden_rate = |selected ∩ gold_exclude| / |selected|
score          = 100 · recall · (1 − forbidden_rate)      # same shape as yaml_cases
```

**Caveat baked into the results:** the `yaml_cases` score formula rewards recall
and only penalizes *explicitly-forbidden* files — it does **not** penalize
over-selection directly. That is why produced-output commits average `score`
78.1 while their `precision` is only **0.176**: diffctx includes the needed
files (recall 0.90) but buries them under ~5× as many irrelevant ones, and the
metric barely notices. **Recommendation: add a precision / F1 term** so the
benchmark stops rewarding whole-tree dumps. `precision`, `recall`,
`forbidden_rate` are all stored per record so an F1 gate can be added without
re-labeling.

## Headline results (diffctx 1.10.2, shipped)

| Metric | Value |
|---|---|
| Commits | 109 |
| **Hang** (`#70`, 0 output) | **72 / 109 (66%)** |
| **Over-dump** (`#65`, >20k tokens) | **34 / 109 (31%)** |
| Usable (`ok`) | **3 / 109 (3%)** |
| Mean precision (produced-output commits) | **0.176** |
| Mean recall (produced-output commits) | 0.901 |
| Mean forbidden-rate | 0.130 |
| Mean score, all commits | 28.3 |
| Mean score, produced-output only | 78.1 |
| MD vs YAML tokens (total) | 1.599M vs 1.723M → **MD 7.2% cheaper** |
| Reviewer format verdict | md_better 34 · yaml_better 3 · similar 66 |

**Read:** on real large commits diffctx is unusable two times out of three
(hang), over-dumps most of the rest, and even when it works it is high-recall /
low-precision — it finds the needed files but drowns them in ~47k tokens of
mostly-unrelated context. This is a structural selection problem, not a tuning
knob.

## Top findings (see `quality_report.md` for named examples)

1. **Hang (`#70`) is a correctness bug, not a scale limit.** Hangs occur across
   the whole size spectrum — 2397-file monorepo renames *and* 13-file single-flag
   flips (`sentry/033`, `gitpod/037`, `react-native/034`). Textbook-trivial diffs
   hang. Rules out "too big"; points at a structural trigger (renames /
   generated-file handling / symbol-graph construction on specific file shapes).
2. **Token cost tracks file size, not diff size.** A 47-line pure-rename commit
   (`react-native/005`) renders 24,955 tokens (~530 tokens/changed-line) because
   non-tree-sitter formats (podspecs, `.mm`, `CMakeLists`, generated protobuf /
   `.api` / lockfiles) collapse to one whole-file "chunk" fragment.
3. **Generic-symbol over-selection.** New code with generic identifiers
   (`Props`, `ShadowNodes`, `ComponentDescriptors`) pattern-matches unrelated
   core-engine files of the same name and pulls in whole subsystems
   (`react-native/009`, `035`).
4. **Lost-change inside a working run (`#103` class).** `react-native/027`:
   diffctx's own changed-files header lists three files whose fragment bodies are
   then absent — budget spent on similarly-named untouched companions instead.
5. **Switch the default format to Markdown** — 11:1 reviewer preference, 7.2%
   fewer tokens, no case where YAML was clearly better on real content.

## How to run / extend

The corpus generator and scoring workflow live under
`eval/datasets/real_world_diff/`; immutable data and labels remain here. The
generator is a thin `git diff` + capped `diffctx` loop and scoring is a
`diffctx_realworld_bench.jsonl` recomputation. To
re-score after a diffctx change: regenerate each commit's rendered file set and
recompute precision/recall/forbidden against the frozen `gold_include` /
`gold_exclude` labels. Gold labels are the stable contract; diffctx's selection
is the variable under test. This complements `yaml_cases` (synthetic, exact) with
real upstream diffs (messy, representative).
