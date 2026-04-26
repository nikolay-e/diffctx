# Deviations from Paper

This document tracks implementation behaviors that differ from the
research paper (`docs/Context-Selection-for-Git-Diff/v2/main.tex`).

The list is intentionally short; substantive design choices live in
the paper, not here.

## None currently outstanding

All previous deviations have been folded into the paper or
removed from the implementation:

- **File-importance prior `I(f)` for impact-need scoring.** Constants
  (`GENERATED_CAP = 0.10`, `PERIPHERAL_CAP = 0.15`, `DEFAULT_IMPORTANCE = 1.0`)
  and path/stem patterns now live in `diffctx/src/config/importance.rs`
  with principled rationale; the computation is `diffctx/src/utility/importance.rs::compute_file_importance`.
  Values are fixed from priors, not trained, to avoid overfitting feature
  engineering to a specific benchmark. See paper Section 4.5.1.

- **Edge-category remapping.** `cicd`, `docker`, `kubernetes`, `build_system`
  now map to `EdgeCategory::Config` (commit `68dd336b`); paper Table 2
  reflects this.

- **`Sibling` variant** added to `EdgeCategory` (commit `bd7f2c31`); paper
  Table 2 lists 10 categories explicitly.

- **`terraform.rs`** moved to `diffctx/src/edges/semantic/` (commit `7ab933f8`);
  folder structure now matches the actual category label.

When new deviations arise, prefer folding them into the paper; only
list here items that are deliberately experimental or not yet
described in the paper text.
