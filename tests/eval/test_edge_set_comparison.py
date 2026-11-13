from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

PROJECT_ROOT = Path(__file__).resolve().parents[2]
IDS = [f"bench::repo__{n}" for n in range(40)]
EDGE_ENV = {"DIFFCTX_CELL_EDGES": "all"}
LEGACY_ENV = {"DIFFCTX_CELL_EDGES": "none"}

# W75-E asks whether the facts-based edges are no worse than the legacy builders. A comparison that cannot
# say "worse" is no gate, and one that says "not worse" for two runs that never differed is a false witness:
# each test below feeds the adapter real run directories (checkpoint rows, run_env.json, bench.json) and
# reads its verdict and exit code, as the orchestrator will.


def _row(iid, file_recall, line_recall, selected, language="python", status="ok"):
    return {
        "instance_id": iid,
        "file_recall": file_recall,
        "file_precision": 0.5,
        "line_recall": line_recall,
        "used_tokens": 100,
        "extra": {
            "status": status,
            "language": language,
            "selected_files": selected,
            "engine_version": "2.0.0",
            "nontrivial_file_recall": file_recall,
        },
    }


def _run(tmp_path, name, rows, env, budget=8000):
    d = tmp_path / name
    d.mkdir()
    tag = "_".join(f"{k}-{v}" for k, v in sorted(env.items()))
    (d / f"bench__{tag}.checkpoint.jsonl").write_text("".join(json.dumps(r) + "\n" for r in rows))
    params = {"tau": 0.05, "core_budget_fraction": 0.4, "budget": budget, "scoring": "ego", "extra_env": env}
    (d / "run_env.json").write_text(json.dumps(params))
    return d


def _bench(tmp_path, name, pack=2.0, locate=1.0, version="diffctx 2.0.0", errors=0):
    op = lambda p95: {"n": 30, "errors": errors, "p50": p95 / 2, "p95": p95, "max": p95 * 2}  # noqa: E731
    path = tmp_path / f"{name}.bench.json"
    path.write_text(json.dumps({"diffctx": version, "all": {"pack": op(pack), "locate": op(locate), "impact": op(1.0)}}))
    return path


def _manifest(tmp_path, ids=IDS):
    d = tmp_path / "manifest"
    d.mkdir(exist_ok=True)
    (d / "test_bench.txt").write_text("".join(f"{i}\n" for i in ids))
    return d


def _compare(tmp_path, legacy, cells, bench_legacy=None, bench_cells=None, ids=IDS):
    return subprocess.run(
        [
            sys.executable,
            "-m",
            "eval.analysis.compare_runs",
            "--legacy",
            str(legacy),
            "--cells",
            str(cells),
            "--bench-legacy",
            str(bench_legacy or _bench(tmp_path, "legacy")),
            "--bench-cells",
            str(bench_cells or _bench(tmp_path, "cells")),
            "--manifest-dir",
            str(_manifest(tmp_path, ids)),
        ],
        cwd=PROJECT_ROOT,
        capture_output=True,
        text=True,
    )


def _legacy_rows():
    return [_row(i, 1.0, 1.0, ["a.py"]) for i in IDS]


def _same_but_one(rows):
    changed = [dict(r, extra=dict(r["extra"])) for r in rows]
    changed[0]["extra"]["selected_files"] = ["a.py", "b.py"]
    return changed


def test_two_runs_that_selected_identically_are_no_measurement(tmp_path):
    legacy = _run(tmp_path, "legacy", _legacy_rows(), LEGACY_ENV)
    cells = _run(tmp_path, "cells", _legacy_rows(), EDGE_ENV)

    result = _compare(tmp_path, legacy, cells)

    assert result.returncode == 2, result.stdout + result.stderr
    assert "INVALID" in result.stdout
    assert "no effect" in result.stdout


def test_runs_over_other_instances_than_the_manifest_are_invalid(tmp_path):
    legacy = _run(tmp_path, "legacy", _legacy_rows()[:30], LEGACY_ENV)
    cells = _run(tmp_path, "cells", _same_but_one(_legacy_rows())[:30], EDGE_ENV)

    result = _compare(tmp_path, legacy, cells)

    assert result.returncode == 2, result.stdout + result.stderr
    assert "10 manifest instances missing" in result.stdout


def test_runs_that_differ_beyond_the_edge_environment_are_invalid(tmp_path):
    legacy = _run(tmp_path, "legacy", _legacy_rows(), LEGACY_ENV, budget=8000)
    cells = _run(tmp_path, "cells", _same_but_one(_legacy_rows()), EDGE_ENV, budget=16000)

    result = _compare(tmp_path, legacy, cells)

    assert result.returncode == 2, result.stdout + result.stderr
    assert "budget" in result.stdout


def test_runs_with_one_edge_environment_are_invalid(tmp_path):
    legacy = _run(tmp_path, "legacy", _legacy_rows(), EDGE_ENV)
    cells = _run(tmp_path, "cells", _same_but_one(_legacy_rows()), EDGE_ENV)

    result = _compare(tmp_path, legacy, cells)

    assert result.returncode == 2, result.stdout + result.stderr
    assert "same edge environment" in result.stdout


def test_a_bench_without_locate_samples_is_invalid(tmp_path):
    legacy = _run(tmp_path, "legacy", _legacy_rows(), LEGACY_ENV)
    cells = _run(tmp_path, "cells", _same_but_one(_legacy_rows()), EDGE_ENV)
    bench = _bench(tmp_path, "cells")
    report = json.loads(bench.read_text())
    report["all"]["locate"] = {"n": 0, "errors": 3}
    bench.write_text(json.dumps(report))

    result = _compare(tmp_path, legacy, cells, bench_cells=bench)

    assert result.returncode == 2, result.stdout + result.stderr
    assert "locate" in result.stdout


def test_the_rule_is_printed_before_any_verdict(tmp_path):
    legacy = _run(tmp_path, "legacy", _legacy_rows(), LEGACY_ENV)
    cells = _run(tmp_path, "cells", _legacy_rows(), EDGE_ENV)

    result = _compare(tmp_path, legacy, cells)

    assert result.stdout.index("Decision rule") < result.stdout.index("INVALID")
    assert "-0.05" in result.stdout and "1.10" in result.stdout


class TestWithBootstrap:
    """The paired interval comes from `eval.harness.stats`, which needs the eval dependency group."""

    @pytest.fixture(autouse=True)
    def _numpy(self):
        pytest.importorskip("numpy")

    def test_equal_recall_is_not_worse(self, tmp_path):
        legacy = _run(tmp_path, "legacy", _legacy_rows(), LEGACY_ENV)
        cells = _run(tmp_path, "cells", _same_but_one(_legacy_rows()), EDGE_ENV)

        result = _compare(tmp_path, legacy, cells)

        assert result.returncode == 0, result.stdout + result.stderr
        assert "NOT WORSE" in result.stdout

    def test_lost_recall_is_worse_and_names_the_language_that_lost_it(self, tmp_path):
        legacy_rows = [_row(i, 1.0, 1.0, ["a.py"], "go" if n % 4 == 0 else "python") for n, i in enumerate(IDS)]
        cells_rows = [
            _row(i, 0.0 if n % 4 == 0 else 1.0, 0.0 if n % 4 == 0 else 1.0, ["b.py"], "go" if n % 4 == 0 else "python")
            for n, i in enumerate(IDS)
        ]
        legacy = _run(tmp_path, "legacy", legacy_rows, LEGACY_ENV)
        cells = _run(tmp_path, "cells", cells_rows, EDGE_ENV)

        result = _compare(tmp_path, legacy, cells)

        assert result.returncode == 1, result.stdout + result.stderr
        assert "WORSE" in result.stdout and "NOT WORSE" not in result.stdout
        go_row = next(line for line in result.stdout.splitlines() if line.startswith("| go "))
        assert "-1.000" in go_row

    def test_a_failed_cells_row_counts_as_zero_recall(self, tmp_path):
        cells_rows = [
            _row(i, 0.0, None, [], status="diffctx_fail") if n < 12 else _row(i, 1.0, 1.0, ["a.py"]) for n, i in enumerate(IDS)
        ]
        legacy = _run(tmp_path, "legacy", _legacy_rows(), LEGACY_ENV)
        cells = _run(tmp_path, "cells", cells_rows, EDGE_ENV)

        result = _compare(tmp_path, legacy, cells)

        assert result.returncode == 1, result.stdout + result.stderr
        assert "non-ok rows: legacy 0, cells 12" in result.stdout

    def test_p95_over_the_margin_is_worse_though_recall_is_equal(self, tmp_path):
        legacy = _run(tmp_path, "legacy", _legacy_rows(), LEGACY_ENV)
        cells = _run(tmp_path, "cells", _same_but_one(_legacy_rows()), EDGE_ENV)

        result = _compare(tmp_path, legacy, cells, bench_cells=_bench(tmp_path, "cells", locate=1.2))

        assert result.returncode == 1, result.stdout + result.stderr
        assert "locate p95" in result.stdout
