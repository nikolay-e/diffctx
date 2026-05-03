from __future__ import annotations

from collections.abc import Iterator
from pathlib import Path

import pytest

from benchmarks.adapters import BenchmarkInstance, EvalResult
from benchmarks.adapters.base import BenchmarkAdapter
from benchmarks.adapters.calibrate import (
    GridSpec,
    TrialResult,
    evaluate_grid,
    render_grid_report,
    top_k_trials,
)
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
)


class _StubAdapter(BenchmarkAdapter):
    def __init__(self, name: str, instances: list[BenchmarkInstance]) -> None:
        self.name = name
        self._instances = instances

    def dataset_revision(self) -> str:
        return f"stub://{self.name}"

    def _load_raw(self) -> Iterator[dict]:
        return iter(())

    def _normalize(self, row: dict) -> BenchmarkInstance | None:
        return None

    def load(self) -> Iterator[BenchmarkInstance]:
        yield from self._instances


def _inst(source: str, idx: int, language: str = "python") -> BenchmarkInstance:
    return BenchmarkInstance(
        instance_id=f"{source}::{idx}",
        source_benchmark=source,
        repo=f"owner/{source}-{idx}",
        base_commit=f"{idx:040x}",
        gold_patch="",
        gold_files=frozenset({"f.py"}),
        language=language,
    )


def _stub_eval_fn(instance: BenchmarkInstance, params: RunParams) -> EvalResult:
    """Synthetic outcome: file_recall is a deterministic function of params and source.

    Allows tests to verify grid-sweep ordering and per-benchmark aggregation
    without spawning subprocesses or hitting HuggingFace.
    """
    base = 0.6 + 0.05 * (params.tau * 10) + 0.03 * (params.core_budget_fraction * 10)
    if instance.source_benchmark == "swebench_lite":
        recall = base
    else:
        recall = base * 0.8  # this benchmark stays harder regardless of params
    recall = min(1.0, max(0.0, recall))
    return EvalResult(
        instance_id=instance.instance_id,
        source_benchmark=instance.source_benchmark,
        file_recall=recall,
        file_precision=recall * 0.5,
        used_tokens=int(2000 + params.budget // 10),
        budget=params.budget,
        elapsed_seconds=0.001,
    )


def test_run_params_to_env_includes_both_calibrated_knobs():
    env = RunParams(tau=0.123, core_budget_fraction=0.456).to_env()
    assert env["DIFFCTX_OP_SELECTION_STOPPING_THRESHOLD"] == "0.123"
    assert env["DIFFCTX_OP_SELECTION_CORE_BUDGET_FRACTION"] == "0.456"


def test_run_params_label_is_filename_safe():
    label = RunParams(tau=0.08, core_budget_fraction=0.7, budget=8000, scoring="ppr").label()
    assert "/" not in label
    assert "tau=" in label
    assert "cbf=" in label


def test_read_manifest_strips_blanks_and_whitespace(tmp_path: Path):
    p = tmp_path / "m.txt"
    p.write_text("a::1\n  b::2  \n\n\nc::3\n")
    assert read_manifest(p) == frozenset({"a::1", "b::2", "c::3"})


def test_filter_instances_by_manifest_only_yields_listed_ids():
    a = _StubAdapter("a", [_inst("a", 1), _inst("a", 2)])
    b = _StubAdapter("b", [_inst("b", 1)])
    wanted = frozenset({"a::1", "b::1"})
    result = list(filter_instances_by_manifest([a, b], wanted))
    ids = sorted(i.instance_id for i in result)
    assert ids == ["a::1", "b::1"]


def test_run_eval_set_preserves_order():
    instances = [_inst("a", i) for i in range(5)]
    params = RunParams()
    out = run_eval_set(instances, _stub_eval_fn, params, workers=1)
    assert [r.instance_id for r in out] == [i.instance_id for i in instances]


def test_run_eval_set_parallel_returns_same_count():
    instances = [_inst("a", i) for i in range(10)]
    params = RunParams()
    seq = run_eval_set(instances, _stub_eval_fn, params, workers=1)
    par = run_eval_set(instances, _stub_eval_fn, params, workers=4)
    assert len(seq) == len(par)
    assert sorted(r.instance_id for r in seq) == sorted(r.instance_id for r in par)


def test_grid_spec_emits_cartesian_product():
    spec = GridSpec(tau_values=(0.04, 0.08), core_budget_fraction_values=(0.5, 0.7, 0.9))
    points = list(spec.points())
    assert len(points) == 6
    assert len(spec) == 6
    assert {(p.tau, p.core_budget_fraction) for p in points} == {
        (0.04, 0.5),
        (0.04, 0.7),
        (0.04, 0.9),
        (0.08, 0.5),
        (0.08, 0.7),
        (0.08, 0.9),
    }


def test_evaluate_grid_records_per_trial_aggregates():
    spec = GridSpec(tau_values=(0.04, 0.16), core_budget_fraction_values=(0.5, 0.8))
    instances = [
        _inst("swebench_lite", 1),
        _inst("contextbench", 1),
    ]
    progress: list[int] = []
    trials = evaluate_grid(spec, instances, _stub_eval_fn, on_trial=lambda i, n, t: progress.append(i))
    assert len(trials) == 4
    assert progress == [0, 1, 2, 3]
    for t in trials:
        assert "swebench_lite" in t.per_benchmark
        assert "contextbench" in t.per_benchmark
        assert 0.0 <= t.score <= 1.0


def test_top_k_trials_sorts_by_score_desc():
    p1 = RunParams(tau=0.1, core_budget_fraction=0.5)
    p2 = RunParams(tau=0.2, core_budget_fraction=0.6)
    p3 = RunParams(tau=0.3, core_budget_fraction=0.7)
    trials = [
        TrialResult(p1, {"a": {"file_recall": 0.5}}),
        TrialResult(p2, {"a": {"file_recall": 0.9}}),
        TrialResult(p3, {"a": {"file_recall": 0.7}}),
    ]
    top2 = top_k_trials(trials, k=2)
    assert top2[0].params == p2
    assert top2[1].params == p3


def test_top_k_breaks_ties_by_lower_token_use():
    p1 = RunParams(tau=0.1, core_budget_fraction=0.5)
    p2 = RunParams(tau=0.2, core_budget_fraction=0.6)
    inst = _inst("a", 1)
    r1 = EvalResult(instance_id=inst.instance_id, source_benchmark="a", file_recall=0.8, file_precision=0.4, used_tokens=4000)
    r2 = EvalResult(instance_id=inst.instance_id, source_benchmark="a", file_recall=0.8, file_precision=0.4, used_tokens=2000)
    t1 = TrialResult(p1, {"a": {"file_recall": 0.8}}, raw_results=(r1,))
    t2 = TrialResult(p2, {"a": {"file_recall": 0.8}}, raw_results=(r2,))
    winner = top_k_trials([t1, t2], k=1)[0]
    assert winner.params == p2  # cheaper tokens wins the tie


def test_render_grid_report_includes_best_cell():
    p = RunParams(tau=0.08, core_budget_fraction=0.7)
    trials = [TrialResult(p, {"a": {"file_recall": 0.85}, "b": {"file_recall": 0.90}})]
    report = render_grid_report(trials)
    assert "Calibration grid report" in report
    assert "Best cell" in report
    assert "0.0800" in report or "0.08" in report
    assert "0.85" in report  # min over per-benchmark file_recall


def test_aggregate_test_set_handles_empty_results():
    report = aggregate_test_set("name", [])
    assert report.n == 0
    assert report.fragment_recall is None


def test_aggregate_test_set_averages_metrics():
    rs = [
        EvalResult("x::1", "x", file_recall=0.6, file_precision=0.5, fragment_recall=0.7, used_tokens=1000),
        EvalResult("x::2", "x", file_recall=0.8, file_precision=0.7, fragment_recall=0.9, used_tokens=3000),
    ]
    report = aggregate_test_set("x", rs)
    assert report.n == 2
    assert report.file_recall == pytest.approx(0.7)
    assert report.fragment_recall == pytest.approx(0.8)
    assert report.used_tokens_mean == pytest.approx(2000.0)


def test_render_paper_table_includes_per_benchmark_and_aggregate():
    rs = [
        aggregate_test_set("a", [EvalResult("a::1", "a", file_recall=0.8, file_precision=0.6)]),
        aggregate_test_set("b", [EvalResult("b::1", "b", file_recall=0.6, file_precision=0.5)]),
    ]
    table = render_paper_table(rs)
    assert "| a |" in table
    assert "| b |" in table
    assert "All benchmarks" in table


def test_aggregate_by_language_groups_using_extra_field():
    rs = [
        EvalResult("a::1", "a", file_recall=0.5, file_precision=0.5, extra={"language": "python"}),
        EvalResult("a::2", "a", file_recall=0.7, file_precision=0.6, extra={"language": "python"}),
        EvalResult("b::1", "b", file_recall=0.9, file_precision=0.8, extra={"language": "java"}),
    ]
    agg = aggregate_by_language(rs)
    assert agg["python"]["n"] == pytest.approx(2.0)
    assert agg["python"]["file_recall"] == pytest.approx(0.6)
    assert agg["java"]["n"] == pytest.approx(1.0)


def test_run_eval_set_resume_from_skips_already_recorded(tmp_path: Path):
    instances = [_inst("a", i) for i in range(5)]
    params = RunParams()
    ckpt = tmp_path / "ckpt.jsonl"
    # Pre-populate checkpoint with two completed IDs.
    pre = run_eval_set(instances[:2], _stub_eval_fn, params, workers=1, checkpoint_path=ckpt)
    assert len(pre) == 2

    invoked_ids: list[str] = []

    def _tracking_eval(instance: BenchmarkInstance, p: RunParams) -> EvalResult:
        invoked_ids.append(instance.instance_id)
        return _stub_eval_fn(instance, p)

    rest = run_eval_set(instances, _tracking_eval, params, workers=1, resume_from=ckpt, checkpoint_path=ckpt)
    # Only the unrecorded instances should actually invoke the eval fn.
    assert invoked_ids == ["a::2", "a::3", "a::4"]
    # Returned results include replayed + freshly-computed entries so a
    # fully-resumed run still aggregates over every instance.
    assert {r.instance_id for r in rest} == {"a::0", "a::1", "a::2", "a::3", "a::4"}


def test_run_eval_set_serial_records_exception_as_error(tmp_path: Path):
    def _broken(instance, params):
        raise RuntimeError("synthetic failure")

    results = run_eval_set([_inst("a", 1)], _broken, RunParams(), workers=1)
    assert len(results) == 1
    assert results[0].extra["status"] == "error"
    assert "synthetic failure" in results[0].extra["error"]


def test_runtime_probe_warns_on_low_disk(tmp_path: Path):
    from benchmarks.adapters.runtime_probe import probe_resources

    msgs = probe_resources(min_memory_gb=0.001, repos_dir=tmp_path, min_disk_gb=10**9)
    severities = [m.severity for m in msgs]
    assert "warn" in severities, f"expected a warn-level message, got {[(m.severity, m.message) for m in msgs]}"


def test_runtime_probe_skips_memory_check_off_linux(monkeypatch):
    from benchmarks.adapters import runtime_probe

    # Force the path to a definitely-missing file.
    monkeypatch.setattr(runtime_probe, "Path", lambda p: __import__("pathlib").Path("/nonexistent/proc/meminfo"))
    msgs = runtime_probe.probe_resources(min_memory_gb=999.0)
    assert any(m.severity == "info" and "skipping" in m.message for m in msgs)


def test_render_split_report_includes_platform():
    from benchmarks.adapters.splits import SplitConfig, build_splits, render_split_report

    class _Empty(BenchmarkAdapter):
        name = "empty"

        def dataset_revision(self):
            return "stub://empty"

        def _load_raw(self):
            return iter(())

        def _normalize(self, row):
            return None

        def load(self):
            yield from ()

    cfg = SplitConfig(test_only_adapters=(_Empty(),), calibration_pool_adapters=(), validation_fraction=0.1)
    report = render_split_report(cfg, build_splits(cfg), today="2026-04-29")
    assert "Platform:" in report
    assert "arm64" in report.lower() or "x86" in report.lower() or "platform:" in report.lower()


def test_render_language_table_orders_by_count_desc():
    agg = {
        "python": {"n": 100.0, "file_recall": 0.7, "file_precision": 0.5},
        "java": {"n": 50.0, "file_recall": 0.6, "file_precision": 0.4},
    }
    table = render_language_table(agg)
    py_idx = table.index("python")
    java_idx = table.index("java")
    assert py_idx < java_idx  # higher count first
