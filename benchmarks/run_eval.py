"""Run diffctx evaluation against one manifest.

Example::

    python -m benchmarks.run_eval \\
        --manifest benchmarks/manifests/v1/calibration.txt \\
        --tau 0.12 --core-budget-fraction 0.5 --budget 8000 \\
        --workers 7 \\
        --out results/eval_calibration_t008_c070.json
"""

from __future__ import annotations

import argparse
import json
from dataclasses import asdict
from pathlib import Path

from benchmarks.adapters import BenchmarkAdapter
from benchmarks.adapters.runner import RunParams, filter_instances_by_manifest, read_manifest, run_eval_set
from benchmarks.adapters.runtime_probe import probe_resources, report_and_maybe_exit
from benchmarks.build_splits import default_calibration_pool_adapters, default_test_adapters
from benchmarks.common import repos_dir as default_repos_dir
from benchmarks.diffctx_eval_fn import make_diffctx_eval_fn


def _all_default_adapters() -> tuple[BenchmarkAdapter, ...]:
    return default_test_adapters() + default_calibration_pool_adapters()


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--manifest", type=Path, required=True)
    p.add_argument("--tau", type=float, default=0.12)
    p.add_argument("--core-budget-fraction", type=float, default=0.5)
    p.add_argument("--budget", type=int, default=8000)
    p.add_argument("--scoring", default="hybrid")
    p.add_argument("--workers", type=int, default=40)
    p.add_argument("--out", type=Path, required=True)
    p.add_argument(
        "--repos-dir",
        type=Path,
        default=None,
        help="Where to cache cloned repositories (default: $CB_REPOS_DIR or ~/.cache/contextbench_repos)",
    )
    p.add_argument("--timeout-per-instance", type=float, default=20.0)
    p.add_argument("--resume-from", type=Path, default=None, help="Skip instance_ids in this JSONL checkpoint.")
    p.add_argument("--checkpoint", type=Path, default=None, help="Append each result to this JSONL as it completes.")
    p.add_argument("--min-memory-gb", type=float, default=16.0, help="Pre-flight memory probe threshold.")
    p.add_argument("--min-disk-gb", type=float, default=50.0, help="Pre-flight disk probe threshold for repos cache.")
    args = p.parse_args()

    repo_root = args.repos_dir or default_repos_dir()
    report_and_maybe_exit(probe_resources(min_memory_gb=args.min_memory_gb, repos_dir=repo_root, min_disk_gb=args.min_disk_gb))

    manifest_ids = read_manifest(args.manifest)
    print(f"Manifest: {args.manifest} → {len(manifest_ids)} instance_ids")

    adapters = _all_default_adapters()
    instances = list(filter_instances_by_manifest(adapters, manifest_ids))
    print(f"Resolved {len(instances)} / {len(manifest_ids)} instances from {len(adapters)} adapters")
    missing = manifest_ids - {i.instance_id for i in instances}
    if missing:
        print(f"WARN: {len(missing)} manifest IDs not produced by any adapter (sample: {list(missing)[:3]})")

    params = RunParams(
        tau=args.tau,
        core_budget_fraction=args.core_budget_fraction,
        budget=args.budget,
        scoring=args.scoring,
    )
    print(f"Params: {params.label()}")

    import os as _os

    _os.environ["DIFFCTX_BENCH_TIMEOUT_SEC"] = str(args.timeout_per_instance)

    eval_fn = make_diffctx_eval_fn(repo_root)
    results = run_eval_set(
        instances,
        eval_fn,
        params,
        workers=args.workers,
        timeout_per_instance=args.timeout_per_instance,
        resume_from=args.resume_from,
        checkpoint_path=args.checkpoint,
    )

    args.out.parent.mkdir(parents=True, exist_ok=True)
    payload = {
        "manifest": str(args.manifest),
        "params": asdict(params),
        "n": len(results),
        "results": [asdict(r) for r in results],
    }
    args.out.write_text(json.dumps(payload, indent=2, default=str))
    print(f"Wrote {len(results)} results to {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
