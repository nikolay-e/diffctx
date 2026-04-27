# Parameter Strategy

This document is the contract between the diffctx implementation and the
research paper (`docs/Context-Selection-for-Git-Diff/v2/main.tex`) on
which parameters are calibrated, which are fixed from domain priors, and
which are sensitivity-checked. It exists so reviewers can verify that
"calibrated against benchmark" claims are scoped to a small, defensible
subset rather than every scalar in `diffctx/src/config/`.

## Principle

A parameter is calibrated only when:

1. It sits at the **top of the influence hierarchy** (a single change
   affects many outputs).
2. Its **dimensionality is low enough** that the labeled corpus
   (~600 SWE-bench instances) supports tuning without overfit. Rule of
   thumb: at least ~50 examples per learnable scalar; calibrating
   100+ parameters on 600 examples is overfit by construction.
3. **No principled domain prior** exists. Where a structural reason
   ("`import` is stronger than `siblings`", "static-typed languages
   make `call` edges more reliable than dynamic-typed ones") fixes the
   value, calibration adds noise without signal.

Parameters that fail any of these conditions stay fixed.

## Three Tiers

### Tier 1 — Calibrated (10 scalars)

The per-`EdgeCategory` weights $w_\tau$, defined in
`diffctx/src/config/category_weights.rs`. There are exactly ten
categories — `Semantic`, `Structural`, `Sibling`, `Config`,
`ConfigGeneric`, `Document`, `Similarity`, `History`, `TestEdge`,
`Generic` — and one scalar multiplier per category. Every fine-grained
edge weight from `weights.rs` is multiplied by its category's $w_\tau$
before scoring.

These ten scalars are the **only learnable parameters of the model**
in the corpus-fitting sense. They are calibrated by Bayesian
optimization against the benchmark via the
`DIFFCTX_CATWEIGHT_*` environment variables (no rebuild required).

| Parameter         | Env var                            | Default |
| ----------------- | ---------------------------------- | ------- |
| `w_semantic`      | `DIFFCTX_CATWEIGHT_SEMANTIC`       | 1.0     |
| `w_structural`    | `DIFFCTX_CATWEIGHT_STRUCTURAL`     | 1.0     |
| `w_sibling`       | `DIFFCTX_CATWEIGHT_SIBLING`        | 1.0     |
| `w_config`        | `DIFFCTX_CATWEIGHT_CONFIG`         | 1.0     |
| `w_config_gen`    | `DIFFCTX_CATWEIGHT_CONFIGGENERIC`  | 1.0     |
| `w_document`      | `DIFFCTX_CATWEIGHT_DOCUMENT`       | 1.0     |
| `w_similarity`    | `DIFFCTX_CATWEIGHT_SIMILARITY`     | 1.0     |
| `w_history`       | `DIFFCTX_CATWEIGHT_HISTORY`        | 1.0     |
| `w_test_edge`     | `DIFFCTX_CATWEIGHT_TESTEDGE`       | 1.0     |
| `w_generic`       | `DIFFCTX_CATWEIGHT_GENERIC`        | 1.0     |

### Tier 1.5 — Per-instance solver (1 mechanism)

The Boltzmann inverse temperature $\beta$ in
`utility/boltzmann.rs` is **not corpus-calibrated**: it is solved
per-instance by binary search to make the soft-budget marginal
distribution exactly fill the requested token budget. Bisection bounds
(`beta_lo`, `beta_hi`), iteration cap, and convergence tolerance are in
`config/selection.rs::BoltzmannConfig` and behave as numerical-method
parameters, not learnable knobs.

### Tier 2 — Domain priors (~265 scalars, fixed)

These encode structural knowledge about how source code relates and
should not be tuned against any benchmark. Calibrating them on 600
examples would yield ~2.4 examples per parameter — overfit by
construction.

- **Per-edge-type weights** (~130, `config/weights.rs`,
  `config/edge_weights.rs`): one default weight per fine-grained edge
  type (e.g. `import`, `inherits`, `same_crate`, `dockerfile_from`).
  Reflect the structural strength of a relation. **Fixed.**
- **Per-language weights** (~90, `config/weights.rs::LANG_WEIGHTS`):
  18 languages × ~5 parameters scaling call/type/usage edges per the
  language's static-vs-dynamic-typing properties. **Fixed.**
- **Need priorities and match strengths** (~30, `config/needs.rs`):
  priorities for need types (`call_definition_priority=1.0`,
  `background_priority=0.2`) and match strengths
  (`defines_scope_match=1.0`, `mentions_fallback=0.3`). Reflect
  semantic importance of need-resolution patterns. **Fixed.**
- **File-importance prior** (`utility/importance.rs`,
  `LIMITS.peripheral_cap`, etc.): structural prior on file roles
  (entrypoints, tests, generated). **Fixed.**

Reviewers asking "did you tune these against the benchmark?" — the
answer is **no, by design**. They are domain priors; the only learned
component scaling them is the 10 $w_\tau$ in Tier 1.

### Tier 3 — Operational, sensitivity-checked (~15 scalars)

These have meaningful influence on output but are not low-dimensional
enough nor isolated enough to justify per-corpus calibration. They are
set from analytical reasoning (PPR damping conventions, ego-graph
locality assumptions, density-greedy stopping heuristics) and verified
by a **±25% / ±50% sensitivity sweep** (`scripts/sensitivity_check.sh`)
that quantifies how much output changes under perturbation.

All Tier-3 parameters are runtime-overridable via `DIFFCTX_OP_*`
environment variables to enable the sweep without rebuild.

| Parameter                                        | Env var                                                  | Default |
| ------------------------------------------------ | -------------------------------------------------------- | ------- |
| `PPR.alpha` (damping $\alpha$)                   | `DIFFCTX_OP_PPR_ALPHA`                                   | 0.60    |
| `PPR.forward_blend` ($\rho$)                     | `DIFFCTX_OP_PPR_FORWARD_BLEND`                           | 0.40    |
| `EGO.per_hop_decay` ($\gamma$)                   | `DIFFCTX_OP_EGO_PER_HOP_DECAY`                           | 1.0     |
| `UTILITY.eta` ($\eta$)                           | `DIFFCTX_OP_UTILITY_ETA`                                 | 0.20    |
| `UTILITY.structural_bonus_weight`                | `DIFFCTX_OP_UTILITY_STRUCTURAL_BONUS_WEIGHT`             | 0.10    |
| `UTILITY.r_cap_sigma`                            | `DIFFCTX_OP_UTILITY_R_CAP_SIGMA`                         | 2.0     |
| `UTILITY.proximity_decay`                        | `DIFFCTX_OP_UTILITY_PROXIMITY_DECAY`                     | 0.30    |
| `SELECTION.core_budget_fraction` ($\beta_{core}$) | `DIFFCTX_OP_SELECTION_CORE_BUDGET_FRACTION`              | 0.70    |
| `SELECTION.stopping_threshold` ($\tau$)          | `DIFFCTX_OP_SELECTION_STOPPING_THRESHOLD`                | 0.08    |
| `SELECTION.r_cap_min`                            | `DIFFCTX_OP_SELECTION_R_CAP_MIN`                         | 0.01    |
| `RESCUE.budget_fraction`                         | `DIFFCTX_OP_RESCUE_BUDGET_FRACTION`                      | 0.05    |
| `RESCUE.min_score_percentile`                    | `DIFFCTX_OP_RESCUE_MIN_SCORE_PERCENTILE`                 | 0.80    |
| `FILTERING.proximity_half_decay`                 | `DIFFCTX_OP_FILTERING_PROXIMITY_HALF_DECAY`              | 50.0    |
| `FILTERING.definition_proximity_half_decay`      | `DIFFCTX_OP_FILTERING_DEFINITION_PROXIMITY_HALF_DECAY`   | 5.0     |
| `BOLTZMANN.calibration_tolerance`                | `DIFFCTX_OP_BOLTZMANN_CALIBRATION_TOLERANCE`             | 0.05    |

The `MODE.hybrid_large_candidate_threshold` discrete switch is also
overridable via `DIFFCTX_OP_MODE_HYBRID_LARGE_CANDIDATE_THRESHOLD` for
ablation, though it is not perturbed in the standard sweep.

## Tier ratios

- Calibrated : domain priors : operational ≈ **10 : 265 : 15**
- Calibrated to learnable-corpus-size ratio: 10 vs 600 = 60 examples per
  parameter — comfortably above the rule-of-thumb 50 floor.

This is the sentence reviewers should be able to verify on their own:
**fewer than 4% of the scalars in `config/` are corpus-calibrated.**

## What this means for the paper

Section 4.3 ("Edge-Type Weight Calibration") describes the calibration
of $w_\tau$ only. Section 4.5.1 (file-importance prior) and the
appendix table on EDGE_WEIGHTS describe domain priors that are **fixed
from structural reasoning, not learned**. Sensitivity analysis for
Tier-3 parameters belongs in an appendix (`scripts/sensitivity_check.sh`
output). Anywhere the paper mentions "tuned" or "calibrated", the
referent must be a Tier-1 parameter.

## What changes when

- New edge type / new language → Tier 2 update, no calibration impact.
- New scoring mode / utility term → may add a Tier-3 parameter; document
  it here and add it to the sensitivity sweep before merge.
- New benchmark dataset → re-run Tier-1 Bayesian optimization. Tier-2
  and Tier-3 do not change unless the new dataset reveals systematic
  bias attributable to a specific prior.
