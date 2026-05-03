"""Validation pass: re-evaluate top-K calibration candidates on the
held-out validation manifest, pick the winner.

Example::

    python -m benchmarks.select_final \\
        --candidates results/calibration/grid_v1/top_candidates.json \\
        --manifest benchmarks/manifests/v1/validation.txt \\
        --workers 7 \\
        --out results/calibration/grid_v1/final_choice.json
"""

from __future__ import annotations

import argparse
import json
from dataclasses import asdict
from pathlib import Path

from benchmarks.adapters.calibrate import TrialResult, top_k_trials
from benchmarks.adapters.evaluator import UniversalEvaluator
from benchmarks.adapters.runner import RunParams, filter_instances_by_manifest, read_manifest, run_eval_set
from benchmarks.adapters.runtime_probe import probe_resources, report_and_maybe_exit
from benchmarks.build_splits import default_calibration_pool_adapters, default_test_adapters
from benchmarks.common import repos_dir as default_repos_dir
from benchmarks.diffctx_eval_fn import make_diffctx_eval_fn


def _load_candidates(path: Path) -> list[RunParams]:
    payload = json.loads(path.read_text())
    out: list[RunParams] = []
    for c in payload["candidates"]:
        out.append(
            RunParams(
                tau=float(c["tau"]),
                core_budget_fraction=float(c["core_budget_fraction"]),
                budget=int(c.get("budget", 8000)),
                scoring=str(c.get("scoring", "hybrid")),
            )
        )
    return out


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--candidates", type=Path, required=True)
    p.add_argument("--manifest", type=Path, required=True)
    p.add_argument("--workers", type=int, default=40)
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--repos-dir", type=Path, default=None)
    p.add_argument("--timeout-per-instance", type=float, default=20.0)
    p.add_argument("--min-memory-gb", type=float, default=16.0)
    p.add_argument("--min-disk-gb", type=float, default=50.0)
    p.add_argument("--checkpoint-dir", type=Path, default=None)
    args = p.parse_args()

    repo_root = args.repos_dir or default_repos_dir()
    report_and_maybe_exit(probe_resources(min_memory_gb=args.min_memory_gb, repos_dir=repo_root, min_disk_gb=args.min_disk_gb))

    candidates = _load_candidates(args.candidates)
    print(f"Re-evaluating {len(candidates)} candidates on {args.manifest}")

    manifest_ids = read_manifest(args.manifest)
    adapters = default_test_adapters() + default_calibration_pool_adapters()
    instances = list(filter_instances_by_manifest(adapters, manifest_ids))
    print(f"Validation set: {len(instances)} instances")

    import os as _os

    _os.environ["DIFFCTX_BENCH_TIMEOUT_SEC"] = str(args.timeout_per_instance)

    eval_fn = make_diffctx_eval_fn(repo_root)
    evaluator = UniversalEvaluator()

    trials: list[TrialResult] = []
    for params in candidates:
        ckpt = (args.checkpoint_dir / f"{params.label()}.jsonl") if args.checkpoint_dir else None
        results = run_eval_set(
            instances,
            eval_fn,
            params,
            workers=args.workers,
            timeout_per_instance=args.timeout_per_instance,
            resume_from=ckpt,
            checkpoint_path=ckpt,
        )
        agg = evaluator.aggregate_per_benchmark(results)
        trial = TrialResult(params=params, per_benchmark=agg, raw_results=tuple(results))
        print(f"τ={params.tau} cbf={params.core_budget_fraction} → score={trial.score:.4f}")
        trials.append(trial)

    winner = top_k_trials(trials, k=1)[0]
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(
        json.dumps(
            {
                "winner": asdict(winner.params),
                "winner_score": winner.score,
                "winner_per_benchmark": winner.per_benchmark,
                "all_candidates": [
                    {"params": asdict(t.params), "score": t.score, "per_benchmark": t.per_benchmark} for t in trials
                ],
            },
            indent=2,
            default=str,
        )
    )
    print(f"Winner: τ={winner.params.tau} cbf={winner.params.core_budget_fraction}")
    print(f"Wrote {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
