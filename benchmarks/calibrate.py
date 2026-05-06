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

from benchmarks.adapters.calibrate import (
    GridSpec,
    evaluate_grid_cached,
    render_grid_report,
    top_k_trials,
)
from benchmarks.adapters.runner import filter_instances_by_manifest, read_manifest
from benchmarks.adapters.runtime_probe import probe_resources, report_and_maybe_exit
from benchmarks.build_splits import default_calibration_pool_adapters, default_test_adapters
from benchmarks.common import repos_dir as default_repos_dir
from benchmarks.diffctx_eval_fn import make_diffctx_eval_all_cells_fn


def _parse_floats(s: str) -> tuple[float, ...]:
    return tuple(float(p) for p in s.split(",") if p.strip())


def _prewarm_bare_clones(instances) -> None:
    """Clone every distinct repo serially before the parallel grid runs.

    Cell 1 otherwise pays the full clone cost on its workers — when many
    workers race on `_ensure_bare_cache` they all stall on the same git
    operation and emit clone-fail spam. A single sequential warmup turns
    that cold path into a one-time setup the grid never sees again.
    """
    from concurrent.futures import ThreadPoolExecutor

    from benchmarks.common import _ensure_bare_cache

    seen: dict[str, str] = {}
    for inst in instances:
        if inst.repo in seen:
            continue
        url = str(inst.extra.get("repo_url") or f"https://github.com/{inst.repo}")
        seen[inst.repo] = url
    print(f"Pre-warming {len(seen)} bare clones...", flush=True)
    with ThreadPoolExecutor(max_workers=8) as pool:
        list(pool.map(lambda kv: _ensure_bare_cache(kv[1], kv[0]), seen.items()))
    print("Pre-warm complete.", flush=True)


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
    p.add_argument("--scoring", default="ego")
    p.add_argument("--workers", type=int, default=40)
    p.add_argument("--top-k", type=int, default=3)
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--repos-dir", type=Path, default=None)
    p.add_argument("--timeout-per-instance", type=float, default=20.0)
    p.add_argument("--min-memory-gb", type=float, default=16.0)
    p.add_argument("--min-disk-gb", type=float, default=50.0)
    p.add_argument(
        "--subsample",
        type=int,
        default=None,
        help="If set, randomly subsample N instances from the manifest (deterministic with --subsample-seed).",
    )
    p.add_argument("--subsample-seed", type=int, default=42)
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
    if args.subsample is not None and args.subsample < len(instances):
        import random as _rnd

        rng = _rnd.Random(args.subsample_seed)
        instances = sorted(instances, key=lambda i: i.instance_id)
        rng.shuffle(instances)
        instances = instances[: args.subsample]
        print(f"Subsampled {len(instances)} of {len(manifest_ids)} (seed={args.subsample_seed})")
    else:
        print(f"Resolved {len(instances)} / {len(manifest_ids)} instances")

    _prewarm_bare_clones(instances)

    # Workers read DIFFCTX_BENCH_TIMEOUT_SEC to scope the kill switch
    # to the diffctx call only (excluding git clone / worktree setup).
    import os as _os

    _os.environ["DIFFCTX_BENCH_TIMEOUT_SEC"] = str(args.timeout_per_instance)

    eval_all_cells_fn = make_diffctx_eval_all_cells_fn(repo_root)
    args.out.mkdir(parents=True, exist_ok=True)
    checkpoint_dir = args.out / "checkpoints"

    def _on_trial(idx: int, total: int, trial) -> None:
        print(
            f"[{idx + 1}/{total}] τ={trial.params.tau:.4f} "
            f"cbf={trial.params.core_budget_fraction:.4f} → "
            f"min={trial.score:.4f}  mean={trial.score_mean:.4f}"
        )

    trials = evaluate_grid_cached(
        spec,
        instances,
        eval_all_cells_fn,
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
                "score_mean": t.score_mean,
                "per_benchmark": t.per_benchmark,
            }
            for t in trials
        ],
    }
    (args.out / "grid_results.json").write_text(json.dumps(payload, indent=2, default=str))

    top = top_k_trials(trials, k=args.top_k)
    top_payload = {
        "top_k": args.top_k,
        "candidates": [asdict(t.params) for t in top],
        "candidate_scores": [{"params": asdict(t.params), "score_min": t.score, "score_mean": t.score_mean} for t in top],
    }
    (args.out / "top_candidates.json").write_text(json.dumps(top_payload, indent=2, default=str))
    print(f"\nTop {args.top_k} candidates by min(per_benchmark file_recall) [mean shown for cherry-pick check]:")
    for t in top:
        print(f"  τ={t.params.tau} cbf={t.params.core_budget_fraction}  min={t.score:.4f}  mean={t.score_mean:.4f}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
