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
from typing import Any

from benchmarks.adapters.evaluator import UniversalEvaluator
from benchmarks.adapters.final_eval import (
    aggregate_by_language,
    aggregate_test_set,
    render_language_table,
    render_paper_table,
)
from benchmarks.adapters.runner import (
    RunParams,
    filter_instances_by_manifest,
    read_manifest,
    run_eval_set,
    run_eval_set_multi_budget,
)
from benchmarks.adapters.runtime_probe import probe_resources, report_and_maybe_exit
from benchmarks.build_splits import default_calibration_pool_adapters, default_test_adapters
from benchmarks.common import repos_dir as default_repos_dir
from benchmarks.diffctx_eval_fn import make_diffctx_eval_all_cells_fn, make_diffctx_eval_fn


def _make_eval_fn(baseline: str, repo_root: Path, request_timeout: float):
    if baseline == "diffctx":
        return make_diffctx_eval_fn(repo_root)
    if baseline == "bm25":
        from benchmarks.baselines.bm25_baseline import make_bm25_eval_fn

        return make_bm25_eval_fn(repo_root)
    if baseline in {"aider", "aider_fair"}:
        from benchmarks.baselines.aider_baseline import make_aider_eval_fn

        return make_aider_eval_fn(repo_root, request_timeout=request_timeout, aider_mode="fair")
    if baseline == "aider_oracle":
        from benchmarks.baselines.aider_baseline import make_aider_eval_fn

        return make_aider_eval_fn(repo_root, request_timeout=request_timeout, aider_mode="oracle")
    raise ValueError(f"unknown baseline: {baseline}")


def _load_winner(path: Path) -> RunParams:
    payload = json.loads(path.read_text())
    w = payload["winner"]
    return RunParams(
        tau=float(w["tau"]),
        core_budget_fraction=float(w["core_budget_fraction"]),
        budget=int(w.get("budget", 8000)),
        scoring=str(w.get("scoring", "ego")),
    )


def _sweep_dir(out: Path, name: str, depth: int | None) -> Path:
    """Per-(manifest, depth) sweep subdirectory for budget-sharded checkpoints."""
    base = out / f"{name}_budget_sweep"
    if depth is not None:
        base = base / f"L{depth}"
    base.mkdir(parents=True, exist_ok=True)
    return base


def _process_manifest(
    manifest_path: Path,
    adapters: Any,
    args: argparse.Namespace,
    params: RunParams,
    budgets_list: list[int],
    eval_fn: Any,
    eval_all_cells_fn: Any,
    depth: int | None,
) -> list[Any]:
    name = manifest_path.stem.removeprefix("test_")
    ids = read_manifest(manifest_path)
    instances = [i for i in filter_instances_by_manifest(adapters, ids) if i.source_benchmark == name]
    # Sort by (repo, base_commit) so consecutive worker tasks reuse the same
    # git worktree — `ensure_repo` keeps a per-worker worktree path keyed on
    # repo_name and skips the worktree-add when the same repo lands twice in
    # a row. SWE-bench-style benchmarks have ~12 instances per repo on
    # average; this saves on the order of (n_unique_repos x worktree_add_cost)
    # per cell, which is several minutes for large repos like django/keras.
    instances.sort(key=lambda i: (i.repo, i.base_commit))
    if args.limit:
        instances = instances[: args.limit]
    depth_label = f" L={depth}" if depth is not None else ""
    print(f"\n[{name}{depth_label}] {len(instances)} instances")

    if eval_all_cells_fn is not None:
        params_list = [
            RunParams(
                tau=params.tau,
                core_budget_fraction=params.core_budget_fraction,
                budget=b,
                scoring=params.scoring,
            )
            for b in budgets_list
        ]
        ckpt_dir = _sweep_dir(args.out, name, depth)
        results_by_budget = run_eval_set_multi_budget(
            instances,
            eval_all_cells_fn,
            params_list,
            workers=args.workers,
            timeout_per_instance=args.timeout_per_instance,
            resume_dir=ckpt_dir,
            checkpoint_dir=ckpt_dir,
        )
        headline_budget = params.budget if params.budget in results_by_budget else budgets_list[-1]
        return results_by_budget[headline_budget]

    if len(budgets_list) > 1:
        ckpt_dir = _sweep_dir(args.out, name, depth)
        results_by_budget: dict[int, list[Any]] = {}
        for b in budgets_list:
            cell_params = RunParams(
                tau=params.tau,
                core_budget_fraction=params.core_budget_fraction,
                budget=b,
                scoring=params.scoring,
            )
            ckpt_b = ckpt_dir / f"b{b}.checkpoint.jsonl"
            rs = run_eval_set(
                instances,
                eval_fn,
                cell_params,
                workers=args.workers,
                timeout_per_instance=args.timeout_per_instance,
                resume_from=ckpt_b,
                checkpoint_path=ckpt_b,
            )
            results_by_budget[b] = rs
        headline_budget = params.budget if params.budget in results_by_budget else budgets_list[-1]
        return results_by_budget[headline_budget]

    depth_suffix = f"_L{depth}" if depth is not None else ""
    ckpt = args.out / f"{name}{depth_suffix}.checkpoint.jsonl"
    return run_eval_set(
        instances,
        eval_fn,
        params,
        workers=args.workers,
        timeout_per_instance=args.timeout_per_instance,
        resume_from=ckpt,
        checkpoint_path=ckpt,
    )


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--winner", type=Path, required=True)
    p.add_argument("--manifests-dir", type=Path, required=True)
    p.add_argument("--workers", type=int, default=40)
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--repos-dir", type=Path, default=None)
    p.add_argument("--timeout-per-instance", type=float, default=20.0)
    p.add_argument("--min-memory-gb", type=float, default=16.0)
    p.add_argument("--min-disk-gb", type=float, default=50.0)
    p.add_argument("--limit", type=int, default=0, help="Cap instances per manifest (0 = all)")
    p.add_argument(
        "--baseline",
        choices=["diffctx", "bm25", "aider", "aider_fair", "aider_oracle"],
        default="diffctx",
        help="Which method to evaluate. Non-diffctx baselines ignore τ/cbf/scoring "
        "(budget is the only RunParam they consume). 'aider' is alias for 'aider_fair'.",
    )
    p.add_argument(
        "--budgets",
        type=str,
        default="",
        help="Comma-separated budgets (e.g. '-1,0,8000,16000,32000,64000,128000'). "
        "When set with --baseline=diffctx, runs the full grid in a single sweep "
        "with compute_scored_state reuse across budgets (~5-7x faster than running "
        "each budget as a separate process). Output: <name>__b<budget>.checkpoint.jsonl. "
        "Empty (default): use winner.budget as a single cell with the legacy path.",
    )
    p.add_argument(
        "--depths",
        type=str,
        default="",
        help="Comma-separated EGO graph traversal depths (e.g. '0,1,2,3,4'). "
        "When set with --baseline=diffctx and --scoring=ego, the orchestrator "
        "loops over each depth as the outer axis and reuses --budgets within "
        "each depth. Heavy phase (graph build + scoring) is re-run per depth "
        "because rel_scores depend on the traversal radius; budgets within a "
        "depth share scored state. Output: <name>_budget_sweep/L<depth>/b<budget>.checkpoint.jsonl. "
        "Empty (default): single depth from MODE.ego_depth_extended (= 2 unless "
        "DIFFCTX_OP_GRAPH_DEPTH is set in the calling shell). Non-EGO scoring "
        "modes ignore --depths (PPR uses alpha; BM25 has no graph traversal).",
    )
    args = p.parse_args()

    repo_root = args.repos_dir or default_repos_dir()
    report_and_maybe_exit(probe_resources(min_memory_gb=args.min_memory_gb, repos_dir=repo_root, min_disk_gb=args.min_disk_gb))

    params = _load_winner(args.winner)
    print(f"Method: {args.baseline} | budget={params.budget} τ={params.tau} cbf={params.core_budget_fraction}")

    manifests = sorted(args.manifests_dir.glob("test_*.txt"))
    if not manifests:
        print(f"No test_*.txt in {args.manifests_dir}")
        return 1

    adapters = default_test_adapters() + default_calibration_pool_adapters()

    import os as _os

    _os.environ["DIFFCTX_BENCH_TIMEOUT_SEC"] = str(args.timeout_per_instance)

    budgets_list: list[int] = []
    if args.budgets.strip():
        budgets_list = [int(x.strip()) for x in args.budgets.split(",") if x.strip()]
        if args.baseline != "diffctx":
            # The reuse optimization is diffctx-specific; bm25/aider have no
            # shared state across budgets. Fall back to per-budget loops.
            print(
                f"--budgets set with non-diffctx baseline ({args.baseline}); "
                "looping per budget without compute_scored_state reuse."
            )

    depths_list: list[int] = []
    if args.depths.strip():
        depths_list = [int(x.strip()) for x in args.depths.split(",") if x.strip()]
        if args.baseline != "diffctx" or params.scoring != "ego":
            print(f"--depths set but baseline={args.baseline} scoring={params.scoring}; " "depths only affect EGO. Ignoring.")
            depths_list = []

    eval_fn = _make_eval_fn(args.baseline, repo_root, request_timeout=args.timeout_per_instance)
    eval_all_cells_fn = (
        make_diffctx_eval_all_cells_fn(repo_root) if args.baseline == "diffctx" and len(budgets_list) > 1 else None
    )

    args.out.mkdir(parents=True, exist_ok=True)
    reports = []
    all_results = []

    # When --depths is set, loop EGO traversal radius as the outer axis.
    # Heavy phase (graph build + scoring) re-runs per depth because
    # rel_scores depend on radius; budgets within a depth share scored state.
    # When --depths is empty, the depth is whatever DIFFCTX_OP_GRAPH_DEPTH
    # the parent shell set (default 2 from MODE.ego_depth_extended).
    loop_depths: list[int | None] = list(depths_list) if depths_list else [None]
    for depth in loop_depths:
        if depth is not None:
            _os.environ["DIFFCTX_OP_GRAPH_DEPTH"] = str(depth)
            print(f"\n=== Sweep depth L={depth} (DIFFCTX_OP_GRAPH_DEPTH={depth}) ===")
        for manifest_path in manifests:
            results = _process_manifest(
                manifest_path=manifest_path,
                adapters=adapters,
                args=args,
                params=params,
                budgets_list=budgets_list,
                eval_fn=eval_fn,
                eval_all_cells_fn=eval_all_cells_fn,
                depth=depth,
            )
            name = manifest_path.stem.removeprefix("test_")
            for r in results:
                r.extra.setdefault("benchmark_manifest", name)
                if depth is not None:
                    r.extra.setdefault("ego_depth", depth)
            all_results.extend(results)
            report = aggregate_test_set(name, results)
            reports.append(report)
            depth_suffix = f"_L{depth}" if depth is not None else ""
            (args.out / f"{name}{depth_suffix}.json").write_text(json.dumps([asdict(r) for r in results], indent=2, default=str))

    paper_table = render_paper_table(reports)
    lang_agg = aggregate_by_language(all_results)
    lang_table = render_language_table(lang_agg)

    extra = f", τ={params.tau}, cbf={params.core_budget_fraction}, scoring={params.scoring}" if args.baseline == "diffctx" else ""
    header = f"# Final evaluation — {args.baseline}\n\nMethod: **{args.baseline}**, budget={params.budget}{extra}"
    summary = "\n\n".join(
        [
            header,
            "## Per-benchmark",
            paper_table,
            "## Per-language",
            lang_table,
        ]
    )
    (args.out / "PAPER_TABLE.md").write_text(summary)
    print(f"\nWrote per-benchmark JSON + PAPER_TABLE.md to {args.out}")

    # Aider keeps a long-lived helper subprocess; shut it down cleanly.
    if hasattr(eval_fn, "shutdown"):
        try:
            eval_fn.shutdown()
        except Exception:
            pass

    UniversalEvaluator()  # touch import to keep linter happy in stub envs
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
