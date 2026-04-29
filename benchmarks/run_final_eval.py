"""Final evaluation: run the winning parameters on every test_*.txt
manifest, emit the paper Section 5 table.

Example::

    python -m benchmarks.run_final_eval \\
        --winner results/calibration/grid_v1/final_choice.json \\
        --manifests-dir benchmarks/manifests/v1 \\
        --workers 7 \\
        --out results/final/v1
"""

from __future__ import annotations

import argparse
import json
from dataclasses import asdict
from pathlib import Path

from benchmarks.adapters.evaluator import UniversalEvaluator
from benchmarks.adapters.final_eval import (
    aggregate_by_language,
    aggregate_test_set,
    render_language_table,
    render_paper_table,
)
from benchmarks.adapters.runner import RunParams, filter_instances_by_manifest, read_manifest, run_eval_set
from benchmarks.adapters.runtime_probe import probe_resources, report_and_maybe_exit
from benchmarks.build_splits import default_calibration_pool_adapters, default_test_adapters
from benchmarks.common import repos_dir as default_repos_dir
from benchmarks.diffctx_eval_fn import make_diffctx_eval_fn


def _load_winner(path: Path) -> RunParams:
    payload = json.loads(path.read_text())
    w = payload["winner"]
    return RunParams(
        tau=float(w["tau"]),
        core_budget_fraction=float(w["core_budget_fraction"]),
        budget=int(w.get("budget", 8000)),
        scoring=str(w.get("scoring", "hybrid")),
    )


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--winner", type=Path, required=True)
    p.add_argument("--manifests-dir", type=Path, required=True)
    p.add_argument("--workers", type=int, default=1)
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--repos-dir", type=Path, default=None)
    p.add_argument("--timeout-per-instance", type=float, default=300.0)
    p.add_argument("--min-memory-gb", type=float, default=16.0)
    p.add_argument("--min-disk-gb", type=float, default=50.0)
    args = p.parse_args()

    repo_root = args.repos_dir or default_repos_dir()
    report_and_maybe_exit(probe_resources(min_memory_gb=args.min_memory_gb, repos_dir=repo_root, min_disk_gb=args.min_disk_gb))

    params = _load_winner(args.winner)
    print(f"Final params: τ={params.tau} cbf={params.core_budget_fraction} budget={params.budget}")

    manifests = sorted(args.manifests_dir.glob("test_*.txt"))
    if not manifests:
        print(f"No test_*.txt in {args.manifests_dir}")
        return 1

    adapters = default_test_adapters() + default_calibration_pool_adapters()
    eval_fn = make_diffctx_eval_fn(repo_root)

    args.out.mkdir(parents=True, exist_ok=True)
    reports = []
    all_results = []
    for manifest_path in manifests:
        name = manifest_path.stem.removeprefix("test_")
        ids = read_manifest(manifest_path)
        instances = [i for i in filter_instances_by_manifest(adapters, ids) if i.source_benchmark == name]
        print(f"\n[{name}] {len(instances)} instances")
        ckpt = args.out / f"{name}.checkpoint.jsonl"
        results = run_eval_set(
            instances,
            eval_fn,
            params,
            workers=args.workers,
            timeout_per_instance=args.timeout_per_instance,
            resume_from=ckpt,
            checkpoint_path=ckpt,
        )
        for r in results:
            r.extra.setdefault("benchmark_manifest", name)
        all_results.extend(results)
        report = aggregate_test_set(name, results)
        reports.append(report)
        (args.out / f"{name}.json").write_text(json.dumps([asdict(r) for r in results], indent=2, default=str))

    paper_table = render_paper_table(reports)
    lang_agg = aggregate_by_language(all_results)
    lang_table = render_language_table(lang_agg)

    summary = "\n\n".join(
        [
            "# Final evaluation",
            f"Final parameters: τ={params.tau}, core_budget_fraction={params.core_budget_fraction}, budget={params.budget}, scoring={params.scoring}",
            "## Per-benchmark",
            paper_table,
            "## Per-language",
            lang_table,
        ]
    )
    (args.out / "PAPER_TABLE.md").write_text(summary)
    print(f"\nWrote per-benchmark JSON + PAPER_TABLE.md to {args.out}")
    UniversalEvaluator()  # touch import to keep linter happy in stub envs
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
