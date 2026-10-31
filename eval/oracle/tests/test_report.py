import json
from pathlib import Path

import report


def cell(**fields):
    base = {
        "language": "python", "mode": "impact", "kind": "calls", "repo": "r", "commit": "c1", "truth": 0, "tp": 0, "tp_weak": 0,
        "fn": 0, "fp": 0, "fp_weak": 0, "kind_mismatches": 0, "abstained": False, "fcn": False,
    }  # fmt: skip
    return {**base, **fields}


def test_scip_rows_compute_precision_recall_abstention_and_weak_share():
    rows = report.scip_rows(
        [
            cell(truth=3, tp=2, fn=1, fp=1),
            cell(commit="c2", truth=2, tp=1, fn=1, abstained=True, fcn=True, tp_weak=1, fp_weak=2),
        ]
    )

    assert len(rows) == 1
    row = rows[0]
    assert (row["tier"], row["language"], row["relation"]) == (1, "python", "calls")
    assert row["precision"] == 0.75
    assert row["recall"] == 0.6
    assert row["recall_with_candidates"] == 0.8
    assert row["abstention_share"] == 0.5
    assert row["false_complete_negatives"] == 1
    assert row["weak_per_answer"] == 1.5


def test_completeness_counts_failures_and_exclusions():
    indexes = [
        {"repo": "r", "ok": True, "reason": "ok", "completeness": {"file_ratio": 1.0, "resolved_ratio": 0.5}},
        {"repo": "r", "ok": False, "reason": "incomplete", "completeness": {"file_ratio": 0.2, "resolved_ratio": 0.1}},
    ]
    commits = [{"repo": "r", "status": "measured"}, {"repo": "r", "status": "excluded"}]

    [row] = report.completeness_rows(indexes, commits)

    assert (row["snapshots"], row["indexed_ok"], row["mean_file_ratio"], row["failures"]) == (2, 1, 1.0, {"incomplete": 1})
    assert (row["commits_measured"], row["commits_excluded"]) == (1, 1)


def test_libtest_logs_and_json_lines_are_both_outcomes(tmp_path: Path):
    log = tmp_path / "mm.log"
    log.write_text("running 3 tests\ntest a::b ... ok\ntest c ... FAILED\ntest d ... ignored\n")
    lines = tmp_path / "f.jsonl"
    lines.write_text('{"name": "x", "state": "pass"}\n{"name": "y", "state": "fail", "false_complete_negative": true}\n')

    assert report.outcome_summary(report.read_records(log))["failing"] == ["c"]
    summary = report.outcome_summary(report.read_records(lines))
    assert (summary["total"], summary["false_complete_negatives"], summary["failing"]) == (2, 1, ["y"])


def test_harness_rows_sum_relations_and_flag_unexplained_empty_answers():
    cases = [
        {"mode": "impact", "language": "python", "state": "pass", "targets": [{"target": "a::f", "relation": "calls", "tp": 2, "fp": 0, "fn": 0}]},
        {"mode": "impact", "language": "python", "state": "fail", "empty": True, "disclosed": False,
         "targets": [{"target": "a::g", "relation": "calls", "tp": 0, "fp": 1, "fn": 2}], "absence_claims": ["x"]},
    ]  # fmt: skip

    rows = report.harness_rows(cases)

    [case] = rows["cases"]
    assert (case["cases"], case["states"]) == (2, {"pass": 1, "fail": 1})
    assert (case["false_complete_negatives"], case["absence_claims"]) == (1, 1)
    [relation] = rows["relations"]
    assert (relation["relation"], relation["precision"], relation["recall"]) == ("calls", 0.667, 0.5)


def test_main_writes_markdown_and_json(tmp_path: Path):
    run = tmp_path / "run"
    run.mkdir()
    (run / "cells.jsonl").write_text(json.dumps(cell(truth=1, tp=1)) + "\n")

    assert report.main(["--run", str(run)]) == 0

    assert "| 1 | python | impact | calls | 1 | 1.0 | 1.0 |" in (run / "report.md").read_text()
    assert json.loads((run / "report.json").read_text())["scip"][0]["precision"] == 1.0
