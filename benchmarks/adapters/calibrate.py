from __future__ import annotations

from collections.abc import Callable, Iterable, Iterator
from concurrent.futures import ProcessPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from benchmarks.adapters.base import BenchmarkInstance, EvalResult
from benchmarks.adapters.evaluator import UniversalEvaluator
from benchmarks.adapters.runner import EvalFn, RunParams, run_eval_set


@dataclass(frozen=True)
class GridSpec:
    """2D grid: cartesian product of `tau_values` by `core_budget_fraction_values`."""

    tau_values: tuple[float, ...]
    core_budget_fraction_values: tuple[float, ...]
    budget: int = 8000
    scoring: str = "hybrid"

    def __post_init__(self) -> None:
        if not self.tau_values:
            raise ValueError("tau_values must be non-empty")
        if not self.core_budget_fraction_values:
            raise ValueError("core_budget_fraction_values must be non-empty")

    def points(self) -> Iterator[RunParams]:
        for tau in self.tau_values:
            for cbf in self.core_budget_fraction_values:
                yield RunParams(
                    tau=tau,
                    core_budget_fraction=cbf,
                    budget=self.budget,
                    scoring=self.scoring,
                )

    def __len__(self) -> int:
        return len(self.tau_values) * len(self.core_budget_fraction_values)


@dataclass(frozen=True)
class TrialResult:
    params: RunParams
    per_benchmark: dict[str, dict[str, float]]
    """`benchmark_name -> {metric: value}` from `aggregate_per_benchmark`."""

    raw_results: tuple[EvalResult, ...] = field(default_factory=tuple)

    @property
    def score(self) -> float:
        """Generalization-friendly objective: minimum file_recall across
        benchmarks. Defending the worst-performing benchmark prevents one
        large source from dominating the global mean."""
        if not self.per_benchmark:
            return 0.0
        return min(agg.get("file_recall", 0.0) for agg in self.per_benchmark.values())

    @property
    def score_mean(self) -> float:
        """Macro-mean recall across benchmarks. Reported alongside
        `score` (= min) so the paper can show that the selected cell
        wins by both — defending against a "cherry-picked min" critique
        when the surface is flat."""
        if not self.per_benchmark:
            return 0.0
        recalls = [agg.get("file_recall", 0.0) for agg in self.per_benchmark.values()]
        return sum(recalls) / len(recalls) if recalls else 0.0


TrialCallback = Callable[[int, int, "TrialResult"], None]


def _make_process_pool(workers: int) -> ProcessPoolExecutor:
    import multiprocessing as mp

    from benchmarks.common import _init_worker

    ctx = mp.get_context("spawn")
    p = ProcessPoolExecutor(
        max_workers=workers,
        mp_context=ctx,
        max_tasks_per_child=50,
        initializer=_init_worker,
    )
    list(p.map(int, range(workers)))
    return p


def _run_trial_with_retry(
    instances: list[BenchmarkInstance],
    eval_fn: EvalFn,
    params: RunParams,
    workers: int,
    timeout_per_instance: float,
    ckpt: Path | None,
    pool: ProcessPoolExecutor | None,
) -> tuple[list[EvalResult], ProcessPoolExecutor | None]:
    from concurrent.futures.process import BrokenProcessPool

    while True:
        try:
            results = run_eval_set(
                instances,
                eval_fn,
                params,
                workers=workers,
                timeout_per_instance=timeout_per_instance,
                resume_from=ckpt,
                checkpoint_path=ckpt,
                pool=pool,
            )
            return results, pool
        except BrokenProcessPool:
            if pool is not None:
                try:
                    pool.shutdown(wait=False, cancel_futures=True)
                except Exception:
                    pass
            pool = _make_process_pool(workers) if workers > 1 else None


def evaluate_grid(
    spec: GridSpec,
    instances: list[BenchmarkInstance],
    eval_fn: EvalFn,
    workers: int = 1,
    on_trial: TrialCallback | None = None,
    timeout_per_instance: float = 20.0,
    checkpoint_dir: Path | None = None,
) -> list[TrialResult]:
    evaluator = UniversalEvaluator()
    points = list(spec.points())
    out: list[TrialResult] = []

    pool: ProcessPoolExecutor | None = _make_process_pool(workers) if workers > 1 else None
    try:
        for i, params in enumerate(points):
            ckpt = (checkpoint_dir / f"{params.label()}.jsonl") if checkpoint_dir is not None else None
            results, pool = _run_trial_with_retry(instances, eval_fn, params, workers, timeout_per_instance, ckpt, pool)
            agg = evaluator.aggregate_per_benchmark(results)
            trial = TrialResult(params=params, per_benchmark=agg, raw_results=tuple(results))
            out.append(trial)
            if on_trial is not None:
                on_trial(i, len(points), trial)
    finally:
        if pool is not None:
            pool.shutdown(wait=False, cancel_futures=True)
    return out


EvalAllCellsFn = Callable[
    [BenchmarkInstance, list[RunParams]],
    list[tuple[RunParams, EvalResult]],
]


def _failure_eval(
    instance: BenchmarkInstance,
    params: RunParams,
    status: str,
    error: str,
) -> EvalResult:
    r = EvalResult(
        instance_id=instance.instance_id,
        source_benchmark=instance.source_benchmark,
        file_recall=0.0,
        file_precision=0.0,
        budget=params.budget,
    )
    r.extra["status"] = status
    r.extra["error"] = error
    r.extra["language"] = instance.language
    return r


def _record_cell_results(
    per_cell_results: list[tuple[RunParams, EvalResult]],
    ckpts: dict[str, Path | None],
    results_by_cell: dict[str, list[EvalResult]],
) -> None:
    from benchmarks.adapters.runner import append_checkpoint

    for params, result in per_cell_results:
        lbl = params.label()
        ckpt = ckpts.get(lbl)
        if ckpt is not None:
            append_checkpoint(ckpt, result)
        results_by_cell[lbl].append(result)


def _drain_pebble_pool(
    pool: Any,
    pending: list[tuple[BenchmarkInstance, list[RunParams]]],
    eval_all_cells_fn: EvalAllCellsFn,
    timeout_per_instance: float,
    ckpts: dict[str, Path | None],
    results_by_cell: dict[str, list[EvalResult]],
) -> None:
    from concurrent.futures import TimeoutError as FuturesTimeoutError

    from pebble import ProcessExpired

    futures: dict = {}
    for inst, params_list in pending:
        future = pool.schedule(
            eval_all_cells_fn,
            args=(inst, params_list),
            timeout=timeout_per_instance + 30.0,
        )
        futures[future] = (inst, params_list)

    for future, (inst, params_list) in futures.items():
        try:
            per_cell = future.result()
        except FuturesTimeoutError:
            per_cell = [
                (p, _failure_eval(inst, p, "timeout", f"pebble killed after {timeout_per_instance + 30.0:.0f}s"))
                for p in params_list
            ]
        except ProcessExpired as e:
            if e.exitcode == 137:
                per_cell = [
                    (p, _failure_eval(inst, p, "timeout", f"diffctx exceeded {timeout_per_instance:.0f}s budget"))
                    for p in params_list
                ]
            else:
                per_cell = [(p, _failure_eval(inst, p, "error", f"ProcessExpired exitcode={e.exitcode}")) for p in params_list]
        except Exception as e:
            per_cell = [(p, _failure_eval(inst, p, "error", f"{type(e).__name__}: {e}")) for p in params_list]
        _record_cell_results(per_cell, ckpts, results_by_cell)


def _build_pending_list(
    instances: list[BenchmarkInstance],
    points: list[RunParams],
    done_ids: dict[str, set[str]],
) -> list[tuple[BenchmarkInstance, list[RunParams]]]:
    pending = []
    for inst in instances:
        needed = [p for p in points if inst.instance_id not in done_ids[p.label()]]
        if needed:
            pending.append((inst, needed))
    return pending


def evaluate_grid_cached(
    spec: GridSpec,
    instances: list[BenchmarkInstance],
    eval_all_cells_fn: EvalAllCellsFn,
    workers: int = 1,
    on_trial: TrialCallback | None = None,
    timeout_per_instance: float = 20.0,
    checkpoint_dir: Path | None = None,
) -> list[TrialResult]:
    from pebble import ProcessPool

    from benchmarks.adapters.runner import _load_existing_results, read_checkpoint
    from benchmarks.common import _init_worker

    evaluator = UniversalEvaluator()
    points = list(spec.points())
    points_by_label: dict[str, RunParams] = {p.label(): p for p in points}

    ckpts: dict[str, Path | None] = {
        lbl: (checkpoint_dir / f"{lbl}.jsonl") if checkpoint_dir is not None else None for lbl in points_by_label
    }
    done_ids: dict[str, set[str]] = {lbl: read_checkpoint(c) if c is not None else set() for lbl, c in ckpts.items()}
    results_by_cell: dict[str, list[EvalResult]] = {
        lbl: (_load_existing_results(c, done_ids[lbl]) if c is not None else []) for lbl, c in ckpts.items()
    }
    pending = _build_pending_list(instances, points, done_ids)

    if pending and workers > 1:
        with ProcessPool(max_workers=workers, max_tasks=40, initializer=_init_worker) as pool:
            _drain_pebble_pool(pool, pending, eval_all_cells_fn, timeout_per_instance, ckpts, results_by_cell)
    elif pending:
        for inst, params_list in pending:
            try:
                per_cell = eval_all_cells_fn(inst, params_list)
            except Exception as e:
                per_cell = [(p, _failure_eval(inst, p, "error", f"{type(e).__name__}: {e}")) for p in params_list]
            _record_cell_results(per_cell, ckpts, results_by_cell)

    out: list[TrialResult] = []
    for i, params in enumerate(points):
        cell_results = results_by_cell[params.label()]
        agg = evaluator.aggregate_per_benchmark(cell_results)
        trial = TrialResult(params=params, per_benchmark=agg, raw_results=tuple(cell_results))
        out.append(trial)
        if on_trial is not None:
            on_trial(i, len(points), trial)
    return out


def top_k_trials(trials: Iterable[TrialResult], k: int = 3) -> list[TrialResult]:
    """Pick the k highest-score trials, breaking ties by lower mean tokens."""

    def _key(t: TrialResult) -> tuple[float, float]:
        if t.raw_results:
            avg_tokens = sum(r.used_tokens for r in t.raw_results) / len(t.raw_results)
        else:
            avg_tokens = 0.0
        return (-t.score, avg_tokens)

    return sorted(trials, key=_key)[:k]


def render_grid_report(trials: list[TrialResult]) -> str:
    """Markdown summary: per-trial score + best-cell highlight."""
    if not trials:
        return "# Grid Report\n\n(empty)\n"
    lines: list[str] = ["# Calibration grid report", ""]
    benchmarks = sorted({name for t in trials for name in t.per_benchmark})
    headers = ["tau", "core_budget_fraction", "min_recall", *benchmarks]
    lines.append("| " + " | ".join(headers) + " |")
    lines.append("|" + "|".join(["---"] * len(headers)) + "|")
    sorted_trials = sorted(trials, key=lambda t: -t.score)
    for t in sorted_trials:
        row = [f"{t.params.tau:.4f}", f"{t.params.core_budget_fraction:.4f}", f"{t.score:.4f}"]
        for name in benchmarks:
            r = t.per_benchmark.get(name, {}).get("file_recall")
            row.append(f"{r:.4f}" if r is not None else "—")
        lines.append("| " + " | ".join(row) + " |")
    lines.append("")
    best = sorted_trials[0]
    lines.append("## Best cell")
    lines.append("")
    lines.append(f"- τ = {best.params.tau}")
    lines.append(f"- core_budget_fraction = {best.params.core_budget_fraction}")
    lines.append(f"- min(per-benchmark file_recall) = {best.score:.4f}")
    lines.append("")
    return "\n".join(lines)
