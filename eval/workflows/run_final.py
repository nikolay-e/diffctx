"""Final evaluation: run the winning parameters on every test_*.txt
manifest, emit the paper Section 5 table.

Example::

    python -m eval run-final \\
        --winner results/calibration/grid_v1/final_choice.json \\
        --manifests-dir datasets/eval-splits/v1 \\
        --workers 7 \\
        --out results/final/v1
"""

from __future__ import annotations

import argparse
import json
from dataclasses import asdict
from pathlib import Path
from typing import Any

from eval.datasets.build_splits import default_calibration_pool_adapters, default_test_adapters
from eval.harness.adapters.evaluator import UniversalEvaluator
from eval.harness.adapters.final_eval import (
    aggregate_by_language,
    aggregate_test_set,
    render_language_table,
    render_paper_table,
)
from eval.harness.adapters.runner import (
    RunParams,
    filter_instances_by_manifest,
    read_manifest,
    run_eval_set,
    run_eval_set_multi_budget,
)
from eval.harness.adapters.runtime_probe import probe_resources, report_and_maybe_exit
from eval.harness.common import repos_dir as default_repos_dir
from eval.harness.diffctx_eval_fn import make_diffctx_eval_all_cells_fn, make_diffctx_eval_fn


def _make_eval_fn(baseline: str, repo_root: Path, request_timeout: float):
    if baseline == "diffctx":
        return make_diffctx_eval_fn(repo_root)
    if baseline == "bm25":
        from eval.baselines.bm25_baseline import make_bm25_eval_fn

        return make_bm25_eval_fn(repo_root)
    if baseline == "patch_files":
        from eval.baselines.patch_files_baseline import make_patch_files_eval_fn

        return make_patch_files_eval_fn(repo_root)
    if baseline == "random":
        from eval.baselines.random_baseline import make_random_eval_fn

        return make_random_eval_fn(repo_root)
    if baseline in {"aider", "aider_fair"}:
        from eval.baselines.aider_baseline import make_aider_eval_fn

        return make_aider_eval_fn(repo_root, request_timeout=request_timeout, aider_mode="fair")
    if baseline == "aider_oracle":
        from eval.baselines.aider_baseline import make_aider_eval_fn

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
        extra_env={str(k): str(v) for k, v in (w.get("extra_env") or {}).items()},
    )


def _sweep_dir(out: Path, name: str, depth: int | None) -> Path:
    """Per-(manifest, depth) sweep subdirectory for budget-sharded checkpoints."""
    base = out / f"{name}_budget_sweep"
    if depth is not None:
        base = base / f"L{depth}"
    base.mkdir(parents=True, exist_ok=True)
    return base


def _resolve_manifest_instances(manifests: list[Path], adapters: Any, limit: int) -> dict[Path, list[Any]]:
    """Load every needed adapter exactly ONCE and slice per manifest.

    The previous per-manifest (and per-depth) `filter_instances_by_manifest`
    call streamed and re-normalized ALL adapters' full datasets on every
    pass — including re-streaming Multi-SWE-bench over the network — for
    each manifest x depth combination.
    """
    wanted_names = {m.stem.removeprefix("test_") for m in manifests}
    relevant = [a for a in adapters if a.name in wanted_names]
    skipped = sorted({a.name for a in adapters} - wanted_names)
    if skipped:
        print(f"Skipping adapters not referenced by any manifest: {', '.join(skipped)}")
    all_ids: set[str] = set()
    for m in manifests:
        all_ids |= read_manifest(m)
    by_id = {inst.instance_id: inst for inst in filter_instances_by_manifest(relevant, all_ids)}

    out: dict[Path, list[Any]] = {}
    for m in manifests:
        name = m.stem.removeprefix("test_")
        ids = read_manifest(m)
        instances = [by_id[i] for i in ids if i in by_id and by_id[i].source_benchmark == name]
        # Sort by (repo, base_commit) so consecutive worker tasks reuse the same
        # git worktree — `ensure_repo` keeps a per-worker worktree path keyed on
        # repo_name and skips the worktree-add when the same repo lands twice in
        # a row. SWE-bench-style benchmarks have ~12 instances per repo on
        # average; this saves on the order of (n_unique_repos x worktree_add_cost)
        # per cell, which is several minutes for large repos like django/keras.
        instances.sort(key=lambda i: (i.repo, i.base_commit))
        if limit:
            instances = instances[:limit]
        missing = len(ids) - sum(1 for i in ids if i in by_id)
        if missing:
            print(f"[{name}] WARN: {missing}/{len(ids)} manifest ids not found in adapters")
        out[m] = instances
    return out


def _process_manifest(
    manifest_path: Path,
    instances: list[Any],
    args: argparse.Namespace,
    params: RunParams,
    budgets_list: list[int],
    eval_fn: Any,
    eval_all_cells_fn: Any,
    depth: int | None,
) -> list[Any]:
    name = manifest_path.stem.removeprefix("test_")
    depth_label = f" L={depth}" if depth is not None else ""
    print(f"\n[{name}{depth_label}] {len(instances)} instances")
    # Distinct checkpoint names per method: two baselines sharing one --out
    # must not cross-resume each other's rows.
    ckpt_name = name if args.baseline == "diffctx" else f"{args.baseline}__{name}"

    if eval_all_cells_fn is not None:
        params_list = [
            RunParams(
                tau=params.tau,
                core_budget_fraction=params.core_budget_fraction,
                budget=b,
                scoring=params.scoring,
                extra_env=params.extra_env,
                provenance_dir=params.provenance_dir,
            )
            for b in budgets_list
        ]
        ckpt_dir = _sweep_dir(args.out, ckpt_name, depth)
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
        ckpt_dir = _sweep_dir(args.out, ckpt_name, depth)
        results_by_budget: dict[int, list[Any]] = {}
        for b in budgets_list:
            cell_params = RunParams(
                tau=params.tau,
                core_budget_fraction=params.core_budget_fraction,
                budget=b,
                scoring=params.scoring,
                extra_env=params.extra_env,
                provenance_dir=params.provenance_dir,
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
    ckpt = args.out / f"{ckpt_name}{depth_suffix}.checkpoint.jsonl"
    return run_eval_set(
        instances,
        eval_fn,
        params,
        workers=args.workers,
        timeout_per_instance=args.timeout_per_instance,
        resume_from=ckpt,
        checkpoint_path=ckpt,
    )


def _build_argparser() -> argparse.ArgumentParser:
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
        "--provenance-dir",
        type=Path,
        default=None,
        help="Dump per-instance discovery provenance to <dir>/<instance_id>.jsonl "
        "(DIFFCTX_PROVENANCE_DUMP). Read it with eval.analysis.discovery_attribution "
        "--dump-dir to split gold into selected / surfaced-not-selected / never-surfaced. "
        "Off by default: one line per candidate fragment is far too much to write on "
        "every cell of a sweep.",
    )
    p.add_argument(
        "--baseline",
        choices=["diffctx", "bm25", "patch_files", "random", "aider", "aider_fair", "aider_oracle"],
        default="diffctx",
        help="Which method to evaluate. Non-diffctx baselines ignore τ/cbf/scoring "
        "(budget is the only RunParam they consume). 'aider' is alias for 'aider_fair'. "
        "'patch_files' = changed-files-only floor; 'random' = seeded-random file packing "
        "on the BM25 protocol.",
    )
    p.add_argument(
        "--aider-request-timeout",
        type=float,
        default=600.0,
        help="Per-request wait for the Aider helper subprocess (seconds). Separate from "
        "--timeout-per-instance, which is the diffctx kill-switch and defaults far too "
        "low for Aider repo-map builds on large repos.",
    )
    p.add_argument(
        "--scoring",
        choices=["ego", "ppr", "bm25", "rrf", "pit"],
        default=None,
        help="Override winner.json scoring mode for --baseline=diffctx (e.g. the "
        "internal-BM25 ablation cell: --baseline diffctx --scoring bm25). 'pit' is the "
        "percentile-fusion successor to 'rrf'.",
    )
    p.add_argument(
        "--tau",
        type=float,
        default=None,
        help="Override winner.json tau (0 disables adaptive stopping).",
    )
    p.add_argument(
        "--extra-env",
        action="append",
        default=[],
        metavar="KEY=VAL",
        help="Extra env var for the diffctx pipeline, repeatable (e.g. "
        "--extra-env DIFFCTX_EGO_LEXICAL_EPS=0 --extra-env DIFFCTX_OBJECTIVE=boltzmann). "
        "Applied around the heavy phase in the multi-budget reuse path.",
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
    return p


def _parse_budgets_list(args: argparse.Namespace) -> list[int]:
    if not args.budgets.strip():
        return []
    budgets_list = [int(x.strip()) for x in args.budgets.split(",") if x.strip()]
    if args.baseline != "diffctx":
        # The reuse optimization is diffctx-specific; bm25/aider have no
        # shared state across budgets. Fall back to per-budget loops.
        print(
            f"--budgets set with non-diffctx baseline ({args.baseline}); looping per budget without compute_scored_state reuse."
        )
    return budgets_list


def _parse_depths_list(args: argparse.Namespace, params: RunParams) -> list[int]:
    if not args.depths.strip():
        return []
    depths_list = [int(x.strip()) for x in args.depths.split(",") if x.strip()]
    if args.baseline != "diffctx" or params.scoring != "ego":
        print(f"--depths set but baseline={args.baseline} scoring={params.scoring}; depths only affect EGO. Ignoring.")
        return []
    return depths_list


def _run_depth_manifest_sweep(
    manifest_instances: dict[Path, list],
    args: argparse.Namespace,
    params: RunParams,
    budgets_list: list[int],
    eval_fn,
    eval_all_cells_fn,
    loop_depths: list[int | None],
) -> tuple[list, list, list[str]]:
    """Loop EGO traversal radius as the outer axis, manifests as the inner axis.

    Heavy phase (graph build + scoring) re-runs per depth because rel_scores
    depend on radius; budgets within a depth share scored state. When
    `loop_depths == [None]`, depth is whatever DIFFCTX_OP_GRAPH_DEPTH the
    parent shell set (default 2 from MODE.ego_depth_extended).

    One manifest blowing up must not abort the rest of the sweep — its
    checkpoints are already on disk; the failure is recorded and reported
    at the end (non-zero exit) instead of killing sibling manifests.
    """
    import os as _os

    reports = []
    all_results = []
    failed: list[str] = []
    for depth in loop_depths:
        if depth is not None:
            _os.environ["DIFFCTX_OP_GRAPH_DEPTH"] = str(depth)
            print(f"\n=== Sweep depth L={depth} (DIFFCTX_OP_GRAPH_DEPTH={depth}) ===")
        for manifest_path, instances in manifest_instances.items():
            _run_one_manifest_guarded(
                manifest_path,
                instances,
                args,
                params,
                budgets_list,
                eval_fn,
                eval_all_cells_fn,
                depth,
                reports,
                all_results,
                failed,
            )
    return reports, all_results, failed


def _run_one_manifest_guarded(
    manifest_path: Path,
    instances: list,
    args: argparse.Namespace,
    params: RunParams,
    budgets_list: list[int],
    eval_fn,
    eval_all_cells_fn,
    depth: int | None,
    reports: list,
    all_results: list,
    failed: list[str],
) -> None:
    import traceback as _traceback

    name = manifest_path.stem.removeprefix("test_")
    depth_suffix = f"_L{depth}" if depth is not None else ""
    try:
        results = _process_manifest(
            manifest_path=manifest_path,
            instances=instances,
            args=args,
            params=params,
            budgets_list=budgets_list,
            eval_fn=eval_fn,
            eval_all_cells_fn=eval_all_cells_fn,
            depth=depth,
        )
    except Exception:
        failed.append(f"{name}{depth_suffix}")
        print(f"[{name}{depth_suffix}] MANIFEST FAILED:\n{_traceback.format_exc()}", flush=True)
        return
    for r in results:
        r.extra.setdefault("benchmark_manifest", name)
        if depth is not None:
            r.extra.setdefault("ego_depth", depth)
    all_results.extend(results)
    reports.append(aggregate_test_set(name, results))
    (args.out / f"{name}{depth_suffix}.json").write_text(json.dumps([asdict(r) for r in results], indent=2, default=str))


def _warm_repo_cache(manifest_instances: dict[Path, list]) -> None:
    """Pre-clone every distinct repo before the parallel run. Without this
    the first wave of workers races on cold bare caches: the flock in
    _ensure_bare_cache serializes them, so N-1 workers idle behind each
    first-seen repo's clone (calibrate.py has always prewarmed; run_eval /
    run_final_eval did not)."""
    from eval.harness.common import warm_cache

    unique: dict[tuple[str, str], dict] = {}
    for instances in manifest_instances.values():
        for inst in instances:
            unique[(inst.repo, inst.base_commit)] = {
                "repo": inst.repo,
                "base_commit": inst.base_commit,
                "repo_url": inst.extra.get("repo_url") or f"https://github.com/{inst.repo}.git",
            }
    if unique:
        warm_cache(list(unique.values()))


def _write_paper_summary(args: argparse.Namespace, params: RunParams, reports: list, all_results: list) -> None:
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


def _parse_cli_extra_env(entries: list[str]) -> dict[str, str] | None:
    cli_env: dict[str, str] = {}
    for kv in entries:
        key, sep, val = kv.partition("=")
        if not sep or not key:
            print(f"Bad --extra-env entry (expected KEY=VAL): {kv!r}")
            return None
        cli_env[key] = val
    return cli_env


def _apply_cli_overrides(params: RunParams, args: argparse.Namespace, cli_env: dict[str, str]) -> RunParams:
    prov = str(args.provenance_dir) if args.provenance_dir else None
    if args.scoring is None and args.tau is None and not cli_env and prov is None:
        return params
    if prov:
        Path(prov).mkdir(parents=True, exist_ok=True)
    return RunParams(
        tau=params.tau if args.tau is None else args.tau,
        core_budget_fraction=params.core_budget_fraction,
        budget=params.budget,
        scoring=params.scoring if args.scoring is None else args.scoring,
        extra_env={**params.extra_env, **cli_env},
        provenance_dir=prov,
    )


def main() -> int:
    args = _build_argparser().parse_args()

    repo_root = args.repos_dir or default_repos_dir()
    report_and_maybe_exit(probe_resources(min_memory_gb=args.min_memory_gb, repos_dir=repo_root, min_disk_gb=args.min_disk_gb))

    cli_env = _parse_cli_extra_env(args.extra_env)
    if cli_env is None:
        return 1
    params = _apply_cli_overrides(_load_winner(args.winner), args, cli_env)
    print(
        f"Method: {args.baseline} | budget={params.budget} τ={params.tau} cbf={params.core_budget_fraction} "
        f"scoring={params.scoring} extra_env={params.extra_env or '{}'}"
    )
    if params.provenance_dir:
        print(f"Provenance dumps: {params.provenance_dir}/<instance_id>.jsonl")

    manifests = sorted(args.manifests_dir.glob("test_*.txt"))
    if not manifests:
        print(f"No test_*.txt in {args.manifests_dir}")
        return 1

    adapters = default_test_adapters() + default_calibration_pool_adapters()

    import os as _os

    _os.environ["DIFFCTX_BENCH_TIMEOUT_SEC"] = str(args.timeout_per_instance)

    budgets_list = _parse_budgets_list(args)
    depths_list = _parse_depths_list(args, params)

    from eval.harness.common import prune_dead_worker_worktrees

    prune_dead_worker_worktrees(Path.home() / ".cache" / "contextbench_repos")
    eval_fn = _make_eval_fn(args.baseline, repo_root, request_timeout=args.aider_request_timeout)
    eval_all_cells_fn = (
        make_diffctx_eval_all_cells_fn(repo_root) if args.baseline == "diffctx" and len(budgets_list) > 1 else None
    )

    manifest_instances = _resolve_manifest_instances(manifests, adapters, args.limit)
    _warm_repo_cache(manifest_instances)

    args.out.mkdir(parents=True, exist_ok=True)
    loop_depths: list[int | None] = list(depths_list) if depths_list else [None]
    reports, all_results, failed_manifests = _run_depth_manifest_sweep(
        manifest_instances, args, params, budgets_list, eval_fn, eval_all_cells_fn, loop_depths
    )

    _write_paper_summary(args, params, reports, all_results)

    # Aider keeps a long-lived helper subprocess; shut it down cleanly.
    if hasattr(eval_fn, "shutdown"):
        try:
            eval_fn.shutdown()
        except Exception:
            pass

    UniversalEvaluator()  # touch import to keep linter happy in stub envs
    if failed_manifests:
        print(f"\nFAILED manifests (checkpoints preserved, rerun to resume): {', '.join(failed_manifests)}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
