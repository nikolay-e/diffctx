from __future__ import annotations

from collections.abc import Iterator
from pathlib import Path

import pytest

from eval.harness.adapters import BenchmarkInstance, EvalResult
from eval.harness.adapters.base import BenchmarkAdapter
from eval.harness.adapters.calibrate import GridSpec, TrialResult, evaluate_grid, render_grid_report, top_k_trials
from eval.harness.adapters.final_eval import aggregate_by_language, aggregate_test_set, render_language_table, render_paper_table
from eval.harness.adapters.runner import RunParams, filter_instances_by_manifest, read_manifest, run_eval_set


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


def test_run_params_to_env_emits_cbf_and_extra_env_only():
    env = RunParams(tau=0.123, core_budget_fraction=0.456, extra_env={"DIFFCTX_EGO_LEXICAL_EPS": "0"}).to_env()
    assert env["DIFFCTX_OP_SELECTION_CORE_BUDGET_FRACTION"] == "0.456"
    assert env["DIFFCTX_EGO_LEXICAL_EPS"] == "0"
    assert "DIFFCTX_OP_SELECTION_STOPPING_THRESHOLD" not in env


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
    from eval.harness.adapters.runtime_probe import probe_resources

    msgs = probe_resources(min_memory_gb=0.001, repos_dir=tmp_path, min_disk_gb=10**9)
    severities = [m.severity for m in msgs]
    assert "warn" in severities, f"expected a warn-level message, got {[(m.severity, m.message) for m in msgs]}"


def test_runtime_probe_skips_memory_check_off_linux(monkeypatch):
    from eval.harness.adapters import runtime_probe

    # Force the path to a definitely-missing file.
    monkeypatch.setattr(runtime_probe, "Path", lambda p: __import__("pathlib").Path("/nonexistent/proc/meminfo"))
    msgs = runtime_probe.probe_resources(min_memory_gb=999.0)
    assert any(m.severity == "info" and "skipping" in m.message for m in msgs)


def test_render_split_report_includes_platform():
    from eval.harness.adapters.splits import SplitConfig, build_splits, render_split_report

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


def test_run_cmd_check_false_returns_returncode_on_timeout():
    from eval.harness.common import run_cmd

    r = run_cmd(["sleep", "5"], check=False, timeout=1)
    assert r.returncode == 124
    assert "timeout" in r.stderr


def test_run_cmd_check_true_still_raises_on_timeout():
    import subprocess

    from eval.harness.common import run_cmd

    with pytest.raises(subprocess.TimeoutExpired):
        run_cmd(["sleep", "5"], check=True, timeout=1)


def test_parallel_eval_records_apply_fail_not_garbage_score(tmp_path: Path, monkeypatch):
    import json as _json
    import subprocess

    import eval.harness.common as bench_common
    from eval.harness.common import apply_as_commit, ensure_repo

    # _SHARED_CACHE is resolved at import time; redirect it so the test's
    # bare clone lands under tmp_path instead of the user's real cache.
    cache_root = tmp_path / "cache"
    cache_root.mkdir()
    monkeypatch.setattr(bench_common, "_SHARED_CACHE", cache_root)

    origin = tmp_path / "origin"
    origin.mkdir()
    subprocess.run(["git", "init", "-q"], cwd=origin, check=True)
    (origin / "app.py").write_text("def real_function():\n    return 1\n")
    subprocess.run(["git", "add", "-A"], cwd=origin, check=True)
    subprocess.run(
        ["git", "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "base"],
        cwd=origin,
        check=True,
    )
    base_commit = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=origin, capture_output=True, text=True, check=True
    ).stdout.strip()

    worktrees = tmp_path / "worktrees"
    worktrees.mkdir()
    repo_dir = ensure_repo(str(origin), "t/origin", base_commit, worktrees)
    assert repo_dir is not None

    non_applying_patch = (
        "diff --git a/missing.py b/missing.py\n"
        "--- a/missing.py\n"
        "+++ b/missing.py\n"
        "@@ -1,1 +1,1 @@\n"
        "-nonexistent line\n"
        "+replacement\n"
    )
    assert apply_as_commit(repo_dir, non_applying_patch, "should-fail") is False

    head_after = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=repo_dir, capture_output=True, text=True, check=True
    ).stdout.strip()
    assert head_after == base_commit

    ckpt = tmp_path / "ckpt.jsonl"

    def _apply_gated_eval(instance: BenchmarkInstance, p: RunParams) -> EvalResult:
        r = EvalResult(
            instance_id=instance.instance_id,
            source_benchmark=instance.source_benchmark,
            file_recall=0.0,
            file_precision=0.0,
            budget=p.budget,
        )
        applied = apply_as_commit(repo_dir, non_applying_patch, "gold")
        r.extra["status"] = "ok" if applied else "apply_fail"
        return r

    results = run_eval_set([_inst("a", 1)], _apply_gated_eval, RunParams(), workers=1, checkpoint_path=ckpt)
    assert results[0].extra["status"] == "apply_fail"
    rows = [_json.loads(line) for line in ckpt.read_text().splitlines()]
    assert rows[0]["extra"]["status"] == "apply_fail"


def _init_repo(path: Path) -> None:
    import subprocess

    path.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init", "-q"], cwd=path, check=True)
    subprocess.run(["git", "config", "core.autocrlf", "false"], cwd=path, check=True)
    subprocess.run(["git", "config", "user.email", "t@t"], cwd=path, check=True)
    subprocess.run(["git", "config", "user.name", "t"], cwd=path, check=True)


def _commit_all(path: Path, message: str) -> None:
    import subprocess

    subprocess.run(["git", "add", "-A"], cwd=path, check=True)
    subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", message], cwd=path, check=True)


def _lf_gold_patch(tmp_path: Path, before: str, after: str) -> str:
    """Gold patch generated against LF content — the PolyBench shape of issue #171."""
    import subprocess

    lf_repo = tmp_path / "lf_origin"
    _init_repo(lf_repo)
    (lf_repo / "app.py").write_text(before)
    _commit_all(lf_repo, "base")
    (lf_repo / "app.py").write_text(after)
    return subprocess.run(["git", "diff"], cwd=lf_repo, capture_output=True, text=True, check=True).stdout


def test_apply_gold_patch_reports_strict_mode_on_matching_line_endings(tmp_path: Path):
    from eval.harness.common import apply_gold_patch

    before = "def f():\n    return 1\n"
    after = "def f():\n    return 2\n"
    patch = _lf_gold_patch(tmp_path, before, after)

    repo = tmp_path / "lf_repo"
    _init_repo(repo)
    (repo / "app.py").write_text(before)
    _commit_all(repo, "base")

    outcome = apply_gold_patch(repo, patch, "gold")
    assert outcome.applied is True
    assert outcome.mode == "strict"
    assert (repo / "app.py").read_text() == after


def test_apply_gold_patch_falls_back_to_whitespace_tolerant_on_crlf_repo(tmp_path: Path):
    import subprocess

    from eval.harness.common import apply_gold_patch

    before = "def f():\n    return 1\n    # tail\n"
    after = "def f():\n    return 2\n    # tail\n"
    patch = _lf_gold_patch(tmp_path, before, after)

    repo = tmp_path / "crlf_repo"
    _init_repo(repo)
    (repo / "app.py").write_bytes(before.replace("\n", "\r\n").encode())
    _commit_all(repo, "base")
    base_commit = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=repo, capture_output=True, text=True, check=True
    ).stdout.strip()

    strict = subprocess.run(["git", "apply", "--index", "-"], cwd=repo, input=patch, capture_output=True, text=True)
    assert strict.returncode != 0, "fixture no longer reproduces the CRLF-vs-LF strict apply failure"

    outcome = apply_gold_patch(repo, patch, "gold")
    assert outcome.applied is True
    assert outcome.mode == "ignore_whitespace"

    head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=repo, capture_output=True, text=True, check=True).stdout.strip()
    assert head != base_commit
    changed = subprocess.run(
        ["git", "diff", "--name-only", "HEAD~1..HEAD"], cwd=repo, capture_output=True, text=True, check=True
    ).stdout.split()
    assert changed == ["app.py"]
    assert "return 2" in (repo / "app.py").read_text()


def test_apply_gold_patch_reports_no_mode_when_content_conflicts(tmp_path: Path):
    from eval.harness.common import apply_gold_patch

    repo = tmp_path / "repo"
    _init_repo(repo)
    (repo / "app.py").write_text("def f():\n    return 1\n")
    _commit_all(repo, "base")

    non_applying = (
        "diff --git a/app.py b/app.py\n"
        "--- a/app.py\n"
        "+++ b/app.py\n"
        "@@ -1,1 +1,1 @@\n"
        "-nonexistent line\n"
        "+replacement\n"
    )
    outcome = apply_gold_patch(repo, non_applying, "gold")
    assert outcome.applied is False
    assert outcome.mode is None


def test_checkpoint_rows_keep_fragment_and_line_metrics(tmp_path: Path):
    import json as _json

    from eval.harness.adapters import GoldenFragment
    from eval.harness.adapters.evaluator import SelectionOutput, UniversalEvaluator

    gold = (GoldenFragment(path="f.py", start_line=1, end_line=10),)
    inst = BenchmarkInstance(
        instance_id="frag::1",
        source_benchmark="frag",
        repo="o/r",
        base_commit="0" * 40,
        gold_patch="",
        gold_files=frozenset({"f.py"}),
        language="python",
        gold_fragments=gold,
    )

    def _fragment_eval(instance: BenchmarkInstance, params: RunParams) -> EvalResult:
        selection = SelectionOutput(
            selected_files=frozenset({"f.py"}),
            selected_fragments=(GoldenFragment(path="f.py", start_line=1, end_line=20),),
            used_tokens=500,
        )
        r = UniversalEvaluator().evaluate(instance, selection, budget=params.budget)
        r.extra["status"] = "ok"
        r.extra["apply_mode"] = "ignore_whitespace"
        return r

    ckpt = tmp_path / "frag.checkpoint.jsonl"
    run_eval_set([inst], _fragment_eval, RunParams(), workers=1, checkpoint_path=ckpt)

    row = _json.loads(ckpt.read_text().splitlines()[0])
    assert row["fragment_recall"] == pytest.approx(1.0)
    assert row["fragment_precision"] == pytest.approx(1.0)
    assert row["line_f1"] == pytest.approx(2 / 3)
    assert row["line_precision"] == pytest.approx(0.5)
    assert row["line_recall"] == pytest.approx(1.0)
    assert row["extra"]["apply_mode"] == "ignore_whitespace"

    # A resumed run must replay those metrics instead of zeroing them.
    replayed = run_eval_set([inst], _fragment_eval, RunParams(), workers=1, resume_from=ckpt)
    assert replayed[0].line_precision == pytest.approx(0.5)
    assert replayed[0].line_recall == pytest.approx(1.0)


def test_cell_metrics_summarizes_line_metrics_and_apply_modes():
    from eval.analysis.cell_metrics import compute_cell_summary

    rows = [
        {
            "instance_id": "i1",
            "file_recall": 1.0,
            "file_precision": 0.5,
            "fragment_recall": 1.0,
            "fragment_precision": 1.0,
            "line_f1": 2 / 3,
            "line_precision": 0.5,
            "line_recall": 1.0,
            "extra": {"status": "ok", "apply_mode": "strict"},
        },
        {
            "instance_id": "i2",
            "file_recall": 0.5,
            "file_precision": 0.5,
            "fragment_recall": 0.5,
            "fragment_precision": 0.5,
            "line_f1": 0.5,
            "line_precision": 0.5,
            "line_recall": 0.5,
            "extra": {"status": "ok", "apply_mode": "ignore_whitespace"},
        },
    ]
    summary = compute_cell_summary(rows)
    assert summary["line_precision"]["mean"] == pytest.approx(0.5)
    assert summary["line_recall"]["mean"] == pytest.approx(0.75)
    assert summary["apply_modes"] == {"strict": 1, "ignore_whitespace": 1}


def test_aggregate_sweep_writes_per_instance_csv_with_fragment_and_line_metrics(tmp_path: Path):
    import csv as _csv
    import json as _json

    from eval.analysis.aggregate_sweep import collect_cells, write_instance_csv

    root = tmp_path / "all_cells"
    cell = root / "cell-ego-b8000-L2-polybench500"
    cell.mkdir(parents=True)
    (cell / "polybench500.checkpoint.jsonl").write_text(
        _json.dumps(
            {
                "instance_id": "polybench500::1",
                "file_recall": 0.75,
                "file_precision": 0.5,
                "fragment_recall": 0.6,
                "fragment_precision": 0.4,
                "line_f1": 0.55,
                "line_precision": 0.45,
                "line_recall": 0.7,
                "used_tokens": 1234,
                "elapsed_seconds": 1.5,
                "extra": {"status": "ok", "apply_mode": "ignore_whitespace", "language": "java", "n_gold": 4},
            }
        )
        + "\n"
    )
    (cell / "cell_summary.json").write_text(_json.dumps({"n": 1}))
    (cell / "metadata.json").write_text(
        _json.dumps({"cell": {"method": "ego", "budget": 8000, "depth": 2, "test_set": "polybench500"}})
    )

    cells = collect_cells(root)
    out = tmp_path / "instance_index.csv"
    assert write_instance_csv(cells, out) == 1

    row = next(iter(_csv.DictReader(out.open())))
    assert row["instance_id"] == "polybench500::1"
    assert row["method"] == "ego"
    assert row["depth"] == "2"
    assert row["fragment_recall"] == "0.6"
    assert row["line_f1"] == "0.55"
    assert row["line_precision"] == "0.45"
    assert row["line_recall"] == "0.7"
    assert row["apply_mode"] == "ignore_whitespace"
    assert row["status"] == "ok"


def test_aggregate_sweep_cell_csv_keeps_existing_columns_and_appends_line_metrics(tmp_path: Path):
    import csv as _csv

    from eval.analysis.aggregate_sweep import write_csv

    cells = [
        {
            "method": "ego",
            "budget": 8000,
            "depth": 2,
            "test_set": "polybench500",
            "n_instances": 1,
            "metadata": {},
            "summary": {
                "n": 1,
                "ok": 1,
                "file_recall": {"mean": 0.75},
                "line_f1": {"mean": 0.55, "n_with_gold": 1},
                "line_precision": {"mean": 0.45},
                "line_recall": {"mean": 0.7},
            },
        }
    ]
    out = tmp_path / "cell_index.csv"
    write_csv(cells, out)
    header = out.read_text().splitlines()[0].split(",")
    assert header[:6] == ["method", "budget", "depth", "test_set", "n_instances", "n_ok"]
    assert header.index("mean_file_recall") < header.index("mean_line_f1")
    row = next(iter(_csv.DictReader(out.open())))
    assert row["mean_line_precision"] == "0.45"
    assert row["mean_line_recall"] == "0.7"
    assert row["n_with_line_gold"] == "1"


def test_aggregate_sweep_expands_multi_budget_cells(tmp_path: Path):
    import json as _json

    from eval.analysis.aggregate_sweep import collect_cells

    root = tmp_path / "all_cells"
    multi = root / "cell-ego-bALL-L2-swebench_verified"
    sweep_dir = multi / "swebench_verified_budget_sweep"
    sweep_dir.mkdir(parents=True)
    for b in (0, 8000):
        (sweep_dir / f"b{b}.checkpoint.jsonl").write_text(_json.dumps({"instance_id": "i1", "file_recall": 0.5}) + "\n")
        (multi / f"cell_summary_b{b}.json").write_text(_json.dumps({"n": 1}))
    (multi / "metadata.json").write_text(
        _json.dumps({"cell": {"method": "ego", "budget": "ALL", "depth": 2, "test_set": "swebench_verified"}})
    )

    legacy = root / "cell-aider-b8000-L-1-swebench_verified"
    legacy.mkdir(parents=True)
    (legacy / "swebench_verified.checkpoint.jsonl").write_text(_json.dumps({"instance_id": "i1", "file_recall": 0.4}) + "\n")
    (legacy / "cell_summary.json").write_text(_json.dumps({"n": 1}))
    (legacy / "metadata.json").write_text(
        _json.dumps({"cell": {"method": "aider", "budget": 8000, "depth": -1, "test_set": "swebench_verified"}})
    )

    cells = collect_cells(root)
    assert {(c["method"], c["budget"], c["depth"]) for c in cells} == {("ego", 0, 2), ("ego", 8000, 2), ("aider", 8000, -1)}
    assert all(c["n_instances"] == 1 for c in cells)
    assert all(c["summary"] for c in cells)
