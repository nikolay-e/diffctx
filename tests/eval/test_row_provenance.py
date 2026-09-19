"""Every diffctx row names the engine and the effective configuration it ran
under. The cell metadata's git commit does not see `DIFFCTX_*` overrides; the
configuration hash does, so rows from two runs compare only when it matches."""

from __future__ import annotations

import diffctx
from eval.harness.adapters.base import BenchmarkInstance
from eval.harness.adapters.evaluator import UniversalEvaluator
from eval.harness.adapters.runner import RunParams
from eval.harness.diffctx_eval_fn import _build_eval_result_from_output
from tests.framework.pygit2_backend import Pygit2Repo


def _instance() -> BenchmarkInstance:
    return BenchmarkInstance(
        instance_id="probe::calc",
        source_benchmark="probe",
        repo="probe/calc",
        base_commit="HEAD~1",
        gold_patch="",
        gold_files=frozenset({"src/calc.py"}),
        language="python",
    )


def _row(repo_path, monkeypatch, env: dict[str, str] | None = None):
    for k, v in (env or {}).items():
        monkeypatch.setenv(k, v)
    output = diffctx.build_diff_context(root_dir=str(repo_path), diff_range="HEAD~1", budget_tokens=8000)
    return _build_eval_result_from_output(output, _instance(), RunParams(budget=8000), 0.1, UniversalEvaluator())


def test_rows_carry_engine_version_and_config_hash_that_moves_with_overrides(tmp_path, monkeypatch):
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("src/calc.py", "def add(a, b):\n    return a + b\n")
    repo.commit("initial")
    repo.add_file("src/calc.py", "def add(a, b):\n    return a + b\n\ndef sub(a, b):\n    return a - b\n")
    repo.commit("add sub")

    plain = _row(repo.path, monkeypatch)
    assert plain.extra["engine_version"] == diffctx.__version__
    assert plain.extra["effective_config_hash"]
    assert plain.extra["budget_tokens"] == 8000
    assert plain.extra["tau"] is not None

    overridden = _row(repo.path, monkeypatch, {"DIFFCTX_OP_GRAPH_DEPTH": "3"})
    assert overridden.extra["effective_config_hash"] != plain.extra["effective_config_hash"]
