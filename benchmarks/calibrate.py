"""Run a 2D grid sweep on the calibration manifest, write per-trial
aggregates plus a markdown grid report.

Example::

    python -m benchmarks.calibrate \\
        --manifest benchmarks/manifests/v1/calibration.txt \\
        --tau 0.04,0.08,0.12,0.16 \\
        --core-budget-fraction 0.5,0.6,0.7,0.8 \\
        --budget 8000 --workers 7 \\
        --out results/calibration/grid_v1
"""

from __future__ import annotations

import argparse
import json
from dataclasses import asdict
from pathlib import Path

from benchmarks.adapters.calibrate import GridSpec, evaluate_grid, render_grid_report, top_k_trials
from benchmarks.adapters.runner import filter_instances_by_manifest, read_manifest
from benchmarks.adapters.runtime_probe import probe_resources, report_and_maybe_exit
from benchmarks.build_splits import default_calibration_pool_adapters, default_test_adapters
from benchmarks.common import repos_dir as default_repos_dir
from benchmarks.diffctx_eval_fn import make_diffctx_eval_fn


def _parse_floats(s: str) -> tuple[float, ...]:
    return tuple(float(p) for p in s.split(",") if p.strip())


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--manifest", type=Path, required=True)
    p.add_argument("--tau", default="0.04,0.08,0.12,0.16", help="Comma-separated τ values")
    p.add_argument(
        "--core-budget-fraction",
        default="0.5,0.6,0.7,0.8",
        help="Comma-separated core_budget_fraction values",
    )
    p.add_argument("--budget", type=int, default=8000)
    p.add_argument("--scoring", default="hybrid")
    p.add_argument("--workers", type=int, default=1)
    p.add_argument("--top-k", type=int, default=3)
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--repos-dir", type=Path, default=None)
    p.add_argument("--timeout-per-instance", type=float, default=300.0)
    p.add_argument("--min-memory-gb", type=float, default=16.0)
    p.add_argument("--min-disk-gb", type=float, default=50.0)
    args = p.parse_args()

    repo_root = args.repos_dir or default_repos_dir()
    report_and_maybe_exit(probe_resources(min_memory_gb=args.min_memory_gb, repos_dir=repo_root, min_disk_gb=args.min_disk_gb))

    spec = GridSpec(
        tau_values=_parse_floats(args.tau),
        core_budget_fraction_values=_parse_floats(args.core_budget_fraction),
        budget=args.budget,
        scoring=args.scoring,
    )
    print(f"Grid: {len(spec.tau_values)} by {len(spec.core_budget_fraction_values)} = {len(spec)} cells")

    manifest_ids = read_manifest(args.manifest)
    adapters = default_test_adapters() + default_calibration_pool_adapters()
    instances = list(filter_instances_by_manifest(adapters, manifest_ids))
    print(f"Resolved {len(instances)} / {len(manifest_ids)} instances")

    eval_fn = make_diffctx_eval_fn(repo_root)
    args.out.mkdir(parents=True, exist_ok=True)
    checkpoint_dir = args.out / "checkpoints"

    def _on_trial(idx: int, total: int, trial) -> None:
        print(
            f"[{idx + 1}/{total}] τ={trial.params.tau:.4f} "
            f"cbf={trial.params.core_budget_fraction:.4f} → "
            f"min(per_benchmark file_recall) = {trial.score:.4f}"
        )

    trials = evaluate_grid(
        spec,
        instances,
        eval_fn,
        workers=args.workers,
        on_trial=_on_trial,
        timeout_per_instance=args.timeout_per_instance,
        checkpoint_dir=checkpoint_dir,
    )

    (args.out / "grid_report.md").write_text(render_grid_report(trials))
    payload = {
        "manifest": str(args.manifest),
        "spec": {
            "tau_values": list(spec.tau_values),
            "core_budget_fraction_values": list(spec.core_budget_fraction_values),
            "budget": spec.budget,
            "scoring": spec.scoring,
        },
        "trials": [
            {
                "params": asdict(t.params),
                "score": t.score,
                "per_benchmark": t.per_benchmark,
            }
            for t in trials
        ],
    }
    (args.out / "grid_results.json").write_text(json.dumps(payload, indent=2, default=str))

    top = top_k_trials(trials, k=args.top_k)
    top_payload = {"top_k": args.top_k, "candidates": [asdict(t.params) for t in top]}
    (args.out / "top_candidates.json").write_text(json.dumps(top_payload, indent=2, default=str))
    print(f"\nTop {args.top_k} candidates by min(per_benchmark file_recall):")
    for t in top:
        print(f"  τ={t.params.tau} cbf={t.params.core_budget_fraction} score={t.score:.4f}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
