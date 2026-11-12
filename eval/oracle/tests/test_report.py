import json
from pathlib import Path

import pytest

import report


def cell(**fields):
    base = {
        "language": "python", "mode": "impact", "kind": "calls", "repo": "r", "commit": "c1", "target": "a::f", "truth": 0, "tp": 0, "tp_weak": 0,
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


def write_run(path: Path, cells: list[dict], engine: str | None = "diffctx 9", budget: int | None = None) -> Path:
    path.mkdir()
    (path / "cells.jsonl").write_text("".join(json.dumps(c) + "\n" for c in cells))
    if engine:
        (path / "run.json").write_text(json.dumps({"engine": engine, "budget": budget}))
    return path


def test_the_curve_lists_one_row_per_relation_and_budget_in_budget_order(tmp_path: Path):
    low = write_run(tmp_path / "low", [cell(truth=4, tp=1, fn=3, abstained=True)], budget=2000)
    high = write_run(tmp_path / "high", [cell(truth=4, tp=3, fn=1, fp=1)], budget=200000)

    rows = report.curve_rows([report.load_run(str(high)), report.load_run(str(low))])

    assert [(r["budget"], r["recall"], r["precision"], r["abstention_share"]) for r in rows] == [
        (2000, 0.25, 1.0, 1.0),
        (200000, 0.75, 0.75, 0.0),
    ]
    assert [report.budget_label(r["budget"]) for r in rows] == ["2000", "uncapped"]


def test_a_run_without_run_json_takes_its_budget_from_the_spec(tmp_path: Path):
    old = write_run(tmp_path / "old", [cell(truth=1, tp=1)], engine=None)

    assert report.load_run(f"{old}@uncapped").budget == report.UNCAPPED_BUDGET
    assert report.load_run(f"{old}@4000").budget == 4000
    assert report.load_run(str(old)).budget is None


def test_a_curve_refuses_two_engines_and_two_runs_at_one_budget(tmp_path: Path):
    a = report.load_run(str(write_run(tmp_path / "a", [cell()], engine="diffctx 1", budget=2000)))
    b = report.load_run(str(write_run(tmp_path / "b", [cell()], engine="diffctx 2", budget=4000)))
    c = report.load_run(str(write_run(tmp_path / "c", [cell()], engine="diffctx 1", budget=2000)))

    with pytest.raises(ValueError, match="one engine"):
        report.curve_rows([a, b])
    with pytest.raises(ValueError, match="both at budget 2000"):
        report.curve_rows([a, c])


def test_main_prints_the_curve_section_when_runs_are_given(tmp_path: Path):
    low = write_run(tmp_path / "low", [cell(truth=2, tp=1, fn=1)], budget=2000)
    high = write_run(tmp_path / "high", [cell(truth=2, tp=2)], budget=4000)

    assert report.main(["--run", str(low), "--curve", str(low), "--curve", str(high), "--out", str(tmp_path / "out")]) == 0

    text = (tmp_path / "out" / "report.md").read_text()
    assert "## Budget curve" in text
    assert "| python | impact | calls | 2000 | 2 | 0.5 | 1.0 | 0 | 0.0 |" in text
    assert "| python | impact | calls | 4000 | 2 | 1.0 | 1.0 | 0 | 0.0 |" in text


def test_main_adds_the_paired_section_flags_a_regression_and_lists_thin_relations(tmp_path: Path):
    def run(name: str, tp: int) -> Path:
        directory = tmp_path / name
        directory.mkdir()
        commits = [f"c{i}" for i in range(8)]
        records = [{"repo": "r", "language": "python", "commit": c, "status": "measured"} for c in commits]
        (directory / "commits.jsonl").write_text("".join(json.dumps(x) + "\n" for x in records))
        (directory / "cells.jsonl").write_text(
            "".join(json.dumps(cell(commit=c, truth=2, tp=tp, fn=2 - tp)) + "\n" for c in commits)
        )
        pair = {
            "repo": "r",
            "language": "python",
            "commit": "c0",
            "mode": "impact",
            "kind": "calls",
            "from": "a::g",
            "to": "a::f",
        }
        (directory / "relations.jsonl").write_text(json.dumps({**pair, "verdict": "tp" if tp == 2 else "fn"}) + "\n")
        (directory / "run.json").write_text(json.dumps({"engine": "diffctx 9", "budget": 2000}))
        return directory

    base, head = run("base", 2), run("head", 1)

    assert report.main(["--run", str(head), "--against", str(base), "--out", str(tmp_path / "out")]) == 0

    text = (tmp_path / "out" / "report.md").read_text()
    assert "## Paired bootstrap: head against base" in text
    assert "recall REGRESSION" in text and "read rows" in text
    assert "Regressions: python impact calls: recall; python impact calls: recall_affected." in text
    assert "| a::g | a::f | calls | c0 | tp | fn |" in text
    assert json.loads((tmp_path / "out" / "report.json").read_text())["paired"][0]["result"]["regressions"]


def test_recall_over_the_affected_relations_is_its_own_column_and_older_cells_count_everything_as_affected():
    rows = report.scip_rows(
        [
            cell(truth=4, tp=3, fn=1, truth_affected=2, tp_affected=1),
            cell(commit="c2", truth=2, tp=1, fn=1),
        ]
    )

    [row] = rows
    assert (row["truth_affected"], row["recall_affected"], row["recall"]) == (4, 0.5, 0.667)


def test_false_folds_and_folded_rows_are_summed_per_relation():
    rows = report.scip_rows([cell(truth=2, tp=2, folded=2, false_fold=1), cell(commit="c2", truth=1, tp=1)])

    [row] = rows
    assert (row["folded"], row["false_fold"]) == (2, 1)


def verdicts(tmp_path: Path, cells: list[dict], gates: Path | None = None, extra: list[str] = ()) -> dict[tuple, dict]:
    tmp_path.mkdir(exist_ok=True)
    run = write_run(tmp_path / "run", cells)
    args = ["--run", str(run), "--out", str(tmp_path / "out"), *extra, *(["--gates", str(gates)] if gates else [])]
    assert report.main(args) == 0
    lines = json.loads((tmp_path / "out" / "report.json").read_text())["verdict"]
    return {(line["gate"], line["source"]): line for line in lines}


def edited_gates(tmp_path: Path, old: str, new: str) -> Path:
    text = report.GATES_FILE.read_text()
    assert old in text
    copy = tmp_path / "gates.toml"
    copy.write_text(text.replace(old, new))
    return copy


def test_every_threshold_of_the_gates_file_gets_a_verdict_line_and_missing_input_is_not_a_pass(tmp_path: Path):
    lines = verdicts(tmp_path, [cell(truth=1, tp=1)])

    assert {gate for gate, _ in lines} == set(report.load_gates())
    assert lines[("safety.false_fold", "oracle cells")]["status"] == "pass"
    assert lines[("safety.false_complete_negatives", "oracle cells")]["value"] == 0
    assert lines[("safety.silent_loss", "")]["status"] == "n/a"
    assert lines[("precise.precision", "")]["status"] == "n/a"
    markdown = (tmp_path / "out" / "report.md").read_text()
    assert "| safety.false_fold | oracle cells | 0 | <= 0 | PASS |" in markdown
    assert "No threshold fails" in markdown


def test_a_false_fold_fails_its_gate_and_the_threshold_comes_from_the_file_alone(tmp_path: Path):
    cells = [cell(truth=2, tp=2, folded=1, false_fold=1)]

    assert verdicts(tmp_path, cells)[("safety.false_fold", "oracle cells")]["status"] == "fail"
    assert "Failing: safety.false_fold." in (tmp_path / "out" / "report.md").read_text()

    relaxed = edited_gates(tmp_path, "false_fold_max = 0", "false_fold_max = 1")
    assert verdicts(tmp_path / "again", cells, relaxed)[("safety.false_fold", "oracle cells")]["status"] == "pass"


def test_false_complete_negatives_are_judged_on_the_oracle_cells_the_fault_sweep_and_the_corpus_alike(tmp_path: Path):
    faults = tmp_path / "faults.jsonl"
    faults.write_text('{"name": "x", "state": "fail", "false_complete_negative": true}\n')
    harness = tmp_path / "harness.json"
    harness.write_text(
        json.dumps([{"mode": "impact", "language": "python", "state": "fail", "empty": True, "targets": [{"fn": 1}]}])
    )

    lines = verdicts(tmp_path, [cell(truth=1, tp=1)], extra=["--faults", str(faults), "--harness", str(harness)])

    status = {source: line["status"] for (gate, source), line in lines.items() if gate == "safety.false_complete_negatives"}
    assert status == {"oracle cells": "pass", "fault sweep": "fail", "YAML corpus": "fail"}


def test_the_impact_p95_is_judged_against_the_file(tmp_path: Path):
    bench = tmp_path / "bench.json"
    bench.write_text(json.dumps({"all": {"impact": {"n": 9, "p50": 1.0, "p95": 5.5, "max": 6, "errors": 0}}}))
    cells = [cell(truth=1, tp=1)]

    assert verdicts(tmp_path, cells, extra=["--bench", str(bench)])[("speed.impact_p95_seconds", "bench")]["status"] == "fail"
    relaxed = edited_gates(tmp_path, "impact_p95_seconds_max = 5.0", "impact_p95_seconds_max = 6.0")
    assert (
        verdicts(tmp_path / "again", cells, relaxed, ["--bench", str(bench)])[("speed.impact_p95_seconds", "bench")]["status"]
        == "pass"
    )


def ac5_totals_file(path: Path, found: int, total: int, noise: float) -> Path:
    path.write_text(json.dumps({"all": {"recall": {"all": {"found": found, "total": total}}, "noise": {"share": noise}}}))
    return path


def test_ac5_needs_the_head_not_below_the_base_scored_on_the_same_material_and_noise_under_the_ceiling(tmp_path: Path):
    base = ac5_totals_file(tmp_path / "base.json", 33, 150, 0.83)
    cells = [cell(truth=1, tp=1)]

    def judged(name: str, found: int, total: int, noise: float) -> dict[str, str]:
        head = ac5_totals_file(tmp_path / f"{name}.json", found, total, noise)
        lines = verdicts(tmp_path / name, cells, extra=["--ac5-head", str(head), "--ac5-base", str(base)])
        return {gate: line["status"] for (gate, _), line in lines.items() if gate.startswith("ac5.")}

    assert judged("ok", 34, 150, 0.85) == {"ac5.head_not_below_recomputed_base": "pass", "ac5.noise": "pass"}
    assert judged("lower", 32, 150, 0.85)["ac5.head_not_below_recomputed_base"] == "fail"
    assert judged("noisy", 40, 150, 0.9)["ac5.noise"] == "fail"
    assert judged("other", 40, 152, 0.85)["ac5.head_not_below_recomputed_base"] == "fail"


def test_a_gates_file_key_that_is_no_threshold_is_refused(tmp_path: Path):
    bad = tmp_path / "gates.toml"
    bad.write_text("[safety]\nfalse_fold = 0\n")

    with pytest.raises(ValueError, match="is no threshold"):
        report.load_gates(bad)
