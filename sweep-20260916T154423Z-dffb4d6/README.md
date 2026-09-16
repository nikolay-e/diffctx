# Sweep sweep-20260916T154423Z-dffb4d6

- Run: https://github.com/nikolay-e/diffctx/actions/runs/35117267607 (full mode, Hetzner CCX63, 4 runners)
- Source SHA: dffb4d6f2fe1459db6f1cff6d123c4715580c84d (diffctx 1.16.0 release candidate)
- Manifests: datasets/eval-splits/v1 (500 per test set; contextbench_verified 494 resolved)
- Operating point: each cell's `winner.json` (resolved from the engine at run time)
- Per-instance timeout: 600 s
- Cells: 75, all completed; 214,136 per-instance rows

The run's own aggregate job failed before committing (git identity on the
wrong clone, fixed in 82797dfd), and its tables showed every bm25 and aider
cell as n=0 (summary step missed the baselines' prefixed checkpoint names,
fixed in 698cd6a3). `aggregated/` here was rebuilt from the run's cell
artifacts with the fixed aggregator.

Raw per-cell checkpoints are not committed (≈167 MB compressed); they are the
run's `cell-*` artifacts (90-day retention) and a local copy.

Reading the tables: polybench500 and swebench_verified have no instance whose
gold reaches beyond the patch (0/500 each), so their recall measures
changed-file retention only; contextbench_verified (214/494 such instances)
is the discriminating set.
