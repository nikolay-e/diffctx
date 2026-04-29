from __future__ import annotations

from collections.abc import Callable, Iterable, Iterator
from dataclasses import dataclass, field
from pathlib import Path

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


TrialCallback = Callable[[int, int, "TrialResult"], None]


def evaluate_grid(
    spec: GridSpec,
    instances: list[BenchmarkInstance],
    eval_fn: EvalFn,
    workers: int = 1,
    on_trial: TrialCallback | None = None,
    timeout_per_instance: float = 300.0,
    checkpoint_dir: Path | None = None,
) -> list[TrialResult]:
    """Run every grid point, return per-trial aggregates.

    `on_trial(idx, total, trial)` fires after each completed trial — the CLI
    uses it for progress logging without coupling the pure logic to stdout.

    `checkpoint_dir`, when set, gets one JSONL per trial named after
    `params.label()`; restarting the sweep skips instances already recorded
    inside each trial's checkpoint.
    """
    evaluator = UniversalEvaluator()
    points = list(spec.points())
    out: list[TrialResult] = []
    for i, params in enumerate(points):
        ckpt = (checkpoint_dir / f"{params.label()}.jsonl") if checkpoint_dir is not None else None
        results = run_eval_set(
            instances,
            eval_fn,
            params,
            workers=workers,
            timeout_per_instance=timeout_per_instance,
            resume_from=ckpt,
            checkpoint_path=ckpt,
        )
        agg = evaluator.aggregate_per_benchmark(results)
        trial = TrialResult(params=params, per_benchmark=agg, raw_results=tuple(results))
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
