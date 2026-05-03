# diffctx v1 — Statistical Analysis Preregistration

**Locked on:** 2026-04-30
**Author:** Nikolay Eremeev
**Repo SHA at lock time:** see `git log -1 --format=%H` at commit time
**Frozen manifests:** `benchmarks/manifests/v1/` (commit `5852d3bb`, 2026-04-29)

This document is committed **before** any of the prespecified primary tests
below have been run on the v1 test set with the methods listed in
"Methods Under Comparison". The intent is to fix in advance which tests
are confirmatory (FWER-corrected) and which are exploratory (FDR-controlled
or descriptive only), to avoid post-hoc selection bias.

The numbers already in the working draft (diffctx-hybrid @ B=8000 hybrid
sweep, BM25 baseline @ B=8000) were collected before this prereg lands;
the prereg therefore covers the **remaining** sweep cells (Aider, scoring
ablation, budget curve) and the **headline statistical tests** computed
from the existing diffctx-hybrid + BM25 data plus the new cells.

---

## Methods Under Comparison

1. **diffctx-hybrid** (deployed default; calibrated τ=0.12, β_core=0.5).
2. **diffctx-ppr** (PPR scoring mode in isolation).
3. **diffctx-ego** (bounded ego-network expansion in isolation).
4. **diffctx-bm25** (internal BM25 scoring mode of diffctx).
5. **External BM25** over patch identifiers (file-level retrieval, no graph;
   `benchmarks/baselines/bm25_baseline.py`).
6. **Aider repo-map, fair-input mode** (`mentioned_fnames` restricted to
   files visible in the input diff text — same information diffctx receives).
7. **Aider repo-map, oracle-mentioned mode** (`mentioned_fnames`
   = `instance.gold_files`; **upper-bound stress test, NOT a baseline**).

## Test Sets

- ContextBench Verified (n=500)
- PolyBench-500 (n=500)
- SWE-bench Verified (n=500)

## Budgets

`{-1 (unlimited ceiling), 0 (no-context floor), 8k, 16k, 32k, 64k, 128k}`

`-1` and `0` serve as monotonicity sanity bounds and must report recall in
the expected directions (B=0 → recall ≈ 0, B=-1 → recall ceiling).

## Metric

**Primary:** file-level recall against gold files at fixed token budget
(per-instance recall, then mean / paired bootstrap aggregations).

**Secondary diagnostics** (descriptive only, not used for primary tests):

- Binary recall (any-gold-found indicator)
- Per-cardinality buckets (gold-set size = 1, 2, ≥3)
- File precision (constrained by |G|/k at fixed budget; reported for
  completeness, not optimised against)

---

## Confirmatory Tests (prespecified, FWER-controlled at α=0.05/3 ≈ 0.0167)

Holm-Bonferroni step-down across the three primary tests.

### P1 — diffctx-hybrid vs External BM25 fair, B=8000

- **Hypothesis:** at the same fixed budget on the same manifests with
  paired instances, diffctx-hybrid attains higher mean file recall than
  External BM25.
- **Statistic:** per-instance paired delta in file recall.
- **Estimator:** percentile bootstrap CI on Δ (B = 10 000 resamples,
  seed = 42), Wilcoxon signed-rank for one-sided p-value.
- **Combination across test sets:** Stouffer's Z-method to combine the
  three per-test-set one-sided p-values into a single pooled p-value.
- **Decision rule:** reject H₀ if Stouffer-pooled p < 0.0167 after Holm
  step-down across {P1, P2, P3}.
- **Both-OK filter:** comparison restricted to instances where both
  methods produced a valid selection (status="ok"); BM25 workers=1 re-run
  is scheduled to remove the workers=2 race asymmetry, but the headline
  test runs on the both-OK subset to be apples-to-apples.

### P2 — Scoring-mode ablation: Friedman omnibus + Nemenyi post-hoc

- **Methods:** {diffctx-hybrid, diffctx-ppr, diffctx-ego, diffctx-bm25}
  (k = 4).
- **Per-instance score:** file recall at the matched B = 8000 cell on the
  same 1500 paired instances.
- **Test:** Friedman χ² omnibus (Demšar 2006). If p < α, run Nemenyi
  post-hoc with critical-difference at α = 0.05; the methods whose mean
  ranks differ by more than the CD are considered significantly distinct.
- **Decision rule:** reject H₀ (all modes equivalent) if Friedman p < 0.0167
  after Holm step-down. Critical-difference diagram in the paper appendix.

### P3 — Aider-oracle headroom (one-sided)

- **Hypothesis:** even given oracle-mentioned files (gold_files), Aider
  repo-map does not exceed diffctx-hybrid recall by more than 5% absolute
  on any test set. This bounds how much downstream value file-hint
  information could provide.
- **Sample:** Aider-oracle on Lite-300 stratified subset across the three
  test sets (≈100 instances each).
- **Test:** one-sided paired bootstrap on Δ_aider_oracle - Δ_diffctx_hybrid
  with H₀: Δ ≥ 0.05.
- **Decision rule:** reject H₀ if upper 95% CI bound of Δ < 0.05.

---

## Exploratory Cells (FDR-controlled at q = 0.10)

All other ~80 cells in the matrix:

- 7 budgets × 4 scoring modes × 3 test sets × pairwise comparisons
- BM25-internal vs External BM25 comparison
- Per-language stratification

These report:

- Bootstrap CI on the cell mean (B = 2 000 resamples, seed = 42).
- Pairwise paired bootstrap deltas with **BH-FDR(q = 0.10)** correction
  across the exploratory family (Demšar 2006; BH 1995).
- No raw uncorrected p-values reported in the paper.

---

## Headline Metric for Budget Curve

**Primary curve summary:** AUC of recall vs log₁₀(budget) computed by the
trapezoidal rule on the 5 paying budgets {8k, 16k, 32k, 64k, 128k} (B=-1
and B=0 excluded as ceiling/floor sanity points). Reported per-method as a
single scalar that captures the budget-efficiency trade-off.

**Per-cell secondary:** the recall × budget table is reported as
exploratory descriptive content (CIs only).

---

## Implementation

- `benchmarks/stats.py`:
  - `bootstrap_ci(values, n_iter)`: vectorised, B = 10 000 by default.
  - `paired_bootstrap_delta(before, after, n_iter)`: vectorised.
  - `wilcoxon_paired(before, after)`: scipy backend.
  - `holm_correct(p_values, alpha)`: step-down FWER for primary tests.
  - `bh_fdr(p_values, q)`: step-up FDR for exploratory tests.
  - `friedman_nemenyi(scores)`: omnibus + post-hoc with CD at α = 0.05.
  - `stouffer_combine(p_values, weights)`: pooling across test sets.

## Reproducibility

- Random seed 42 throughout.
- Frozen manifests at `benchmarks/manifests/v1/`.
- Pinned dataset revisions in `benchmarks/dataset_revisions.json`.
- Pinned dependency versions in `requirements-bench.lock`.
- Compute environment: M4 Pro, 48 GB unified memory, macOS arm64,
  Python 3.12.12, Rust 1.92.0.

---

## Sources

- Demšar, J. (2006). Statistical Comparisons of Classifiers over Multiple
  Data Sets. *JMLR* 7:1-30. <https://www.jmlr.org/papers/v7/demsar06a.html>
- Benjamini, Y., Hochberg, Y. (1995). Controlling the False Discovery Rate.
  *JRSS B* 57(1):289-300. <https://www.jstor.org/stable/2346101>
- Andrade, C. (2023). HARKing, Cherry-Picking, P-Hacking, Fishing
  Expeditions, and Data Dredging. *Indian J Psychol Med* 45(1).
  <https://pmc.ncbi.nlm.nih.gov/articles/PMC10964884/>
- Lakens, D. *Improving Your Statistical Inferences*, ch. 13:
  Preregistration. <https://lakens.github.io/statistical_inferences/13-prereg.html>
