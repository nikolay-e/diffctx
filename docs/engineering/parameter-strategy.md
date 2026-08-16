# Parameter Strategy

This document is the contract between the diffctx implementation and the
research paper (`paper/v2/src/main.tex`) on
which parameters are calibrated, which are fixed from domain priors, and
which are sensitivity-checked. It exists so reviewers can verify that
"calibrated against benchmark" claims are scoped to a small, defensible
subset rather than every scalar in `crates/diffctx-native/src/config/`.

## Principle

A parameter is calibrated only when:

1. It sits at the **top of the influence hierarchy** (a single change
   affects many outputs).
2. Its **dimensionality is low enough** that the labeled corpus
   (1500 instances across three frozen 500-instance manifests:
   SWE-bench Verified, PolyBench-500, ContextBench Verified) supports
   tuning without overfit. Rule of thumb: at least ~50 examples per
   learnable scalar; calibrating 100+ parameters on 1500 examples is
   overfit by construction.
3. **No principled domain prior** exists. Where a structural reason
   ("`import` is stronger than `siblings`", "static-typed languages
   make `call` edges more reliable than dynamic-typed ones") fixes the
   value, calibration adds noise without signal.

Parameters that fail any of these conditions stay fixed.

## Three Tiers

### Tier 1 — Calibrated (2 scalars)

The two operational parameters actually calibrated against the corpus
are the stopping threshold $\tau$ and the core budget fraction
$\beta_{core}$ (paper: §"Edge-Type Weight Priors and Operational
Calibration" and §"Calibration Protocol"). Calibration is a 5×3 grid
search over $(\tau, \beta_{core})$ on the frozen calibration manifests,
validated on a held-out manifest. The v2-cycle winner was
$\tau = 0.12$, $\beta_{core} = 0.5$; the v5 cycle re-validated the
operating point together with the per-file admission gate (#65), and
the deployed defaults now carry $\tau = 0.05$, $\beta_{core} = 0.4$:

- $\tau$ — `DEFAULT_STOPPING_THRESHOLD = 0.05` in `config/limits.rs`,
  user-overridable via the `--tau` CLI flag only (no env var). The weak
  stop ships only together with the admission gate — without it,
  $\tau = 0.05$ re-admits the diffuse tail the gate exists to block.
- $\beta_{core}$ — `core_budget_fraction = 0.4` in
  `config/selection.rs`, env-overridable via
  `DIFFCTX_OP_SELECTION_CORE_BUDGET_FRACTION` for sweeps.

#### Per-category weights — uncalibrated unit priors

The per-`EdgeCategory` multipliers $w_\tau$, defined in
`crates/diffctx-native/src/config/category_weights.rs`. There are
exactly ten categories — `Semantic`, `Structural`, `Sibling`, `Config`,
`ConfigGeneric`, `Document`, `Similarity`, `History`, `TestEdge`,
`Generic` — and one scalar multiplier per category. Every fine-grained
edge weight from `weights.rs` is multiplied by its category's $w_\tau$
before scoring.

These ten scalars are the **intended** future calibration surface, but
today they are all fixed at 1.0, have **no runtime override plumbing**
(no env vars, no CLI flags), and **no calibration has ever run** on
them. The paper states the same: full data-driven calibration over the
category-weight simplex is left to future work because the labeled
corpora needed to disambiguate ten weights without overfit exceed the
current evaluation's size. Until that happens, treat them as
uncalibrated unit priors — any claim that they were "learned" or
"tuned" is false.

### Tier 1.5 — Per-instance solver (1 mechanism)

The Boltzmann inverse temperature $\beta$ in
`utility/boltzmann.rs` is **not corpus-calibrated**: it is solved
per-instance by binary search to make the soft-budget marginal
distribution exactly fill the requested token budget. Bisection bounds
(`beta_lo`, `beta_hi`), iteration cap, and convergence tolerance are in
`config/selection.rs::BoltzmannConfig` and behave as numerical-method
parameters, not learnable knobs.

### Tier 2 — Domain priors (~275 scalars, fixed)

These encode structural knowledge about how source code relates and
should not be tuned against any benchmark. Calibrating them on 1500
examples would yield ~5 examples per parameter — an order of magnitude
below the ~50-per-scalar floor, overfit by construction.

- **Per-edge-type weights** (~130, `config/weights.rs`,
  `config/edge_weights.rs`): one default weight per fine-grained edge
  type (e.g. `import`, `inherits`, `same_crate`, `dockerfile_from`).
  Reflect the structural strength of a relation. **Fixed.**
- **Per-category multipliers** (10,
  `config/category_weights.rs`): all 1.0, see Tier 1 above.
  **Fixed pending future calibration.**
- **Per-language weights** (90, `config/weights.rs::LANG_WEIGHTS`):
  18 language entries × 5 fields scaling call/type/usage edges per the
  language's static-vs-dynamic-typing properties. **Fixed.**
- **Need priorities and match strengths** (~30, `config/needs.rs`):
  priorities for need types (`call_definition_priority=1.0`,
  `background_priority=0.2`) and match strengths
  (`defines_scope_match=1.0`, `mentions_fallback=0.3`). Reflect
  semantic importance of need-resolution patterns. **Fixed.**
- **File-importance prior** (`utility/importance.rs`,
  `LIMITS.peripheral_cap`, etc.): structural prior on file roles
  (entrypoints, tests, generated). **Fixed.**

### Tier 3 — Operational, sensitivity-checked (18 scalars)

These have meaningful influence on output but are not low-dimensional
enough nor isolated enough to justify per-corpus calibration. They are
set from analytical reasoning (PPR damping conventions, ego-graph
locality assumptions, density-greedy stopping heuristics) and verified
by a **±25% / ±50% sensitivity sweep** (`scripts/sensitivity_check.py`,
wrapped by `sensitivity_check.sh`) that quantifies how much output
changes under perturbation.

Tier-3 parameters are runtime-overridable via environment variables
(most under the `DIFFCTX_OP_*` prefix, a few historical ones without
it) to enable the sweep without rebuild.

> **Not a stable interface.** The `DIFFCTX_OP_*` overrides — along with the
> other internal toggles (`DIFFCTX_OBJECTIVE`, `DIFFCTX_EGO_*`,
> `DIFFCTX_NO_COMMIT_SIGNAL`, `DIFFCTX_MAX_FRAGMENTS`,
> `DIFFCTX_FILE_ADMISSION`, `DIFFCTX_FILE_STAR`, `DIFFCTX_PIT_SHAPE`,
> `DIFFCTX_PIT_TRANSFORM`, `DIFFCTX_MAX_EDGES_PER_NODE`,
> `DIFFCTX_TRACE_BUILDERS`, and the
> `DIFFCTX_PROVENANCE_DUMP=<path>` per-candidate telemetry sink) — are experimental
> calibration knobs for research and sensitivity analysis, not a supported
> public API. They are undocumented in `--help` on purpose and may change or
> disappear between releases. Production use should rely only on the
> documented CLI flags (`--alpha`, `--tau`, `--budget`, `--scoring`).

| Parameter                                        | Env var                                                  | Default |
| ------------------------------------------------ | -------------------------------------------------------- | ------- |
| `PPR.alpha` (damping $\alpha$)                   | `DIFFCTX_OP_PPR_ALPHA`                                   | 0.60    |
| `PPR.forward_blend` ($\rho$)                     | `DIFFCTX_OP_PPR_FORWARD_BLEND`                           | 0.40    |
| `EGO.per_hop_decay` ($\gamma$)                   | `DIFFCTX_EGO_PER_HOP_DECAY`                              | 0.5     |
| `EGO.identifier_overlap_epsilon`                 | `DIFFCTX_EGO_LEXICAL_EPS`                                | 0.1     |
| `RRF.k` (rank-fusion damping)                    | `DIFFCTX_RRF_K`                                          | 60.0    |
| `PIT.blend` (structural share of percentile fusion) | `DIFFCTX_PIT_BLEND`                                    | 0.65    |
| `PIT.agreement_bonus` (both-signals-agree bonus) | `DIFFCTX_PIT_AGREEMENT_BONUS`                            | 0.10    |
| `PIT.agreement_top_k` (agreement window)         | `DIFFCTX_PIT_AGREEMENT_TOP_K`                            | 20      |
| `MODE.bm25_top_k_primary` (lexical discovery breadth) | `DIFFCTX_BM25_DISCOVERY_TOP_K`                      | 1       |
| Graph traversal radius                           | `DIFFCTX_OP_GRAPH_DEPTH`                                 | 2       |
| `UTILITY.eta` ($\eta$)                           | `DIFFCTX_OP_UTILITY_ETA`                                 | 0.20    |
| `UTILITY.structural_bonus_weight`                | `DIFFCTX_OP_UTILITY_STRUCTURAL_BONUS_WEIGHT`             | 0.10    |
| `UTILITY.r_cap_sigma`                            | `DIFFCTX_OP_UTILITY_R_CAP_SIGMA`                         | 2.0     |
| `UTILITY.proximity_decay`                        | `DIFFCTX_OP_UTILITY_PROXIMITY_DECAY`                     | 0.30    |
| `SELECTION.r_cap_min`                            | `DIFFCTX_OP_SELECTION_R_CAP_MIN`                         | 0.01    |
| `SELECTION.per_file_budget_fraction`             | `DIFFCTX_OP_SELECTION_PER_FILE_BUDGET_FRACTION`          | 0.25    |
| `RESCUE.budget_fraction`                         | `DIFFCTX_OP_RESCUE_BUDGET_FRACTION`                      | 0.05    |
| `RESCUE.min_score_percentile`                    | `DIFFCTX_OP_RESCUE_MIN_SCORE_PERCENTILE`                 | 0.80    |
| `FILTERING.proximity_half_decay`                 | `DIFFCTX_OP_FILTERING_PROXIMITY_HALF_DECAY`              | 50.0    |
| `FILTERING.definition_proximity_half_decay`      | `DIFFCTX_OP_FILTERING_DEFINITION_PROXIMITY_HALF_DECAY`   | 5.0     |
| `NEEDS.relatedness_bonus`                        | `DIFFCTX_RELATEDNESS_BONUS`                              | 0.25    |
| `NEEDS.min_rel_for_bonus`                        | `DIFFCTX_MIN_REL_FOR_BONUS`                              | 0.03    |
| `BOLTZMANN.calibration_tolerance`                | `DIFFCTX_OP_BOLTZMANN_CALIBRATION_TOLERANCE`             | 0.05    |
| `BOLTZMANN.bisect_iters`                         | `DIFFCTX_OP_BOLTZMANN_BISECT_ITERS`                      | 24      |

The Tier-1 pair ($\tau$, $\beta_{core}$) is intentionally absent from
this table: those two are calibrated, not merely sensitivity-checked.

Fewer than 1% of the scalars in `config/` are corpus-calibrated;
anywhere the paper mentions "tuned" or "calibrated", the referent must
be $\tau$ or $\beta_{core}$.

## What changes when

- New edge type / new language → Tier 2 update, no calibration impact.
- New scoring mode / utility term → may add a Tier-3 parameter; document
  it here and add it to the sensitivity sweep before merge.
- New benchmark dataset → re-run the $(\tau, \beta_{core})$ grid
  calibration. Tier-2 and Tier-3 do not change unless the new dataset
  reveals systematic bias attributable to a specific prior.
- Category-weight calibration becomes feasible (larger labeled corpus)
  → that is a new Tier-1 entry and a Q-class change; it requires env
  plumbing first, then a full calibration → validation → sweep cycle.
