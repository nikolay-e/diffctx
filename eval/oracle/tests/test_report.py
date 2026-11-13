import json
from pathlib import Path

import pytest

import compare
import measure
import report
from gates import GATES_FILE, load_gates


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


def test_main_adds_the_paired_section_fails_a_measured_cell_and_lists_thin_relations(tmp_path: Path):
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
    assert "recall FAILS, recall_affected FAILS, recall WORSE" in text and "read rows" in text
    assert (
        "Cells: 1, measured 1 (failing 2), unmeasured 0. Failing: python impact calls: recall; python impact calls: recall_affected."
        in text
    )
    assert "| a::g | a::f | calls | c0 | tp | fn |" in text
    assert "| python | impact | calls | 8 | 16 | 1.0 | 0.5 |" in text and "| failing |" in text
    result = json.loads((tmp_path / "out" / "report.json").read_text())["paired"][0]["result"]
    assert result["measurability"]["failing"] and result["rows"][0]["state"] == "failing"

    lines = {line["gate"]: line for line in json.loads((tmp_path / "out" / "report.json").read_text())["verdict"]}
    assert (lines["measurability.lower_bound"]["status"], lines["measurability.lower_bound"]["value"]) == ("fail", -0.5)
    width = lines["measurability.interval_width"]
    assert (width["status"], width["value"]) == ("info", "1 of 1 cells measured")
    assert "0 cells judged on the safety rows alone" in width["detail"]
    assert "| measurability.interval_width |" in text and "INFO" in text
    assert "Failing: measurability.lower_bound." in text

    relaxed = edited_gates(tmp_path, "lower_bound_min = -0.05", "lower_bound_min = -0.6")
    assert (
        report.main(["--run", str(head), "--against", str(base), "--out", str(tmp_path / "relaxed"), "--gates", str(relaxed)])
        == 0
    )
    again = json.loads((tmp_path / "relaxed" / "report.json").read_text())
    assert again["paired"][0]["result"]["rows"][0]["state"] == "measured"
    assert {line["gate"]: line["status"] for line in again["verdict"]}["measurability.lower_bound"] == "pass"

    strict = edited_gates(tmp_path, "interval_width_max = 0.20", "interval_width_max = -1")
    assert (
        report.main(["--run", str(head), "--against", str(base), "--out", str(tmp_path / "strict"), "--gates", str(strict)]) == 0
    )
    unmeasured = json.loads((tmp_path / "strict" / "report.json").read_text())
    assert unmeasured["paired"][0]["result"]["rows"][0]["state"] == "unmeasured"
    statuses = {line["gate"]: line["status"] for line in unmeasured["verdict"]}
    assert statuses["measurability.lower_bound"] == "not measured"
    assert statuses["measurability.interval_width"] == "info"  # no width can make it pass or fail
    assert {line["gate"]: line["value"] for line in unmeasured["verdict"]}[
        "measurability.interval_width"
    ] == "0 of 1 cells measured"


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
    text = GATES_FILE.read_text()
    assert old in text
    copy = tmp_path / "gates.toml"
    copy.write_text(text.replace(old, new))
    return copy


def test_every_threshold_of_the_gates_file_gets_a_verdict_line_and_missing_input_is_not_a_pass(tmp_path: Path):
    lines = verdicts(tmp_path, [cell(truth=1, tp=1)])

    assert {gate for gate, _ in lines} == set(load_gates())
    assert lines[("safety.false_fold", "oracle cells")]["status"] == "pass"
    assert lines[("safety.silent_loss", "")]["status"] == "not measured"
    assert lines[("precise.precision", "")]["status"] == "not measured"
    markdown = (tmp_path / "out" / "report.md").read_text()
    assert "| safety.false_fold | oracle cells | 0 | <= 0 | PASS |" in markdown
    assert "No threshold fails" in markdown
    assert "| safety.silent_loss |  | n/a | every row read | NOT MEASURED |" in markdown


def test_a_false_fold_fails_its_gate_and_the_threshold_comes_from_the_file_alone(tmp_path: Path):
    cells = [cell(truth=2, tp=2, folded=1, false_fold=1)]

    assert verdicts(tmp_path, cells)[("safety.false_fold", "oracle cells")]["status"] == "fail"
    assert "Failing: safety.false_fold." in (tmp_path / "out" / "report.md").read_text()

    relaxed = edited_gates(tmp_path, "false_fold_max = 0", "false_fold_max = 1")
    assert verdicts(tmp_path / "again", cells, relaxed)[("safety.false_fold", "oracle cells")]["status"] == "pass"


def test_the_false_complete_negative_has_no_verdict_line_of_its_own_and_the_suites_still_print_it(tmp_path: Path):
    faults = tmp_path / "faults.jsonl"
    faults.write_text('{"name": "x", "state": "fail", "false_complete_negative": true}\n')
    harness = tmp_path / "harness.json"
    harness.write_text(
        json.dumps([{"mode": "impact", "language": "python", "state": "fail", "empty": True, "targets": [{"fn": 1}]}])
    )

    lines = verdicts(tmp_path, [cell(truth=1, tp=1)], extra=["--faults", str(faults), "--harness", str(harness)])

    assert not [gate for gate, _ in lines if "false_complete" in gate]
    assert "safety.incomplete_complete" in {gate for gate, _ in lines}
    assert "false-complete-negatives 1, failing ['x']" in (tmp_path / "out" / "report.md").read_text()


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
        load_gates(bad)


def run_with(path: Path, cells: list[dict], pairs: list[dict] = (), silent: list[dict] = ()) -> Path:
    run = write_run(path, cells)
    for name, rows in (("relations.jsonl", pairs), ("silent_loss.jsonl", silent)):
        if rows:
            (run / name).write_text("".join(json.dumps(row) + "\n" for row in rows))
    return run


def test_the_incomplete_complete_count_is_per_cell_with_every_pair_listed_and_judged_by_the_file(tmp_path: Path):
    cells = [
        cell(truth=2, tp=1, fn=1, incomplete_complete=1),
        cell(commit="c2", kind="reads", truth=2, tp=0, fn=2, incomplete_complete=2),
        cell(commit="c3", truth=1, tp=1, incomplete_complete=0),
    ]
    pair = {"repo": "r", "language": "python", "mode": "impact", "kind": "calls", "to": "a::f"}
    pairs = [
        {**pair, "commit": "c1" * 8, "from": "a::g", "verdict": "fn", "incomplete_complete": True},
        {**pair, "commit": "c1" * 8, "from": "a::h", "verdict": "tp", "incomplete_complete": False},
    ]
    run = run_with(tmp_path / "run", cells, pairs)

    assert report.main(["--run", str(run), "--out", str(tmp_path / "out")]) == 0

    found = json.loads((tmp_path / "out" / "report.json").read_text())
    assert {r["relation"]: r["incomplete_complete"] for r in found["scip"]} == {"calls": 1, "reads": 2}
    assert found["incomplete_complete"]["count"] == 3
    assert [(p["from"], p["commit"]) for p in found["incomplete_complete"]["listing"]] == [("a::g", "c1c1c1c1c1")]
    verdict = {line["gate"]: line for line in found["verdict"]}["safety.incomplete_complete"]
    assert (verdict["status"], verdict["value"], verdict["limit"]) == ("fail", 3, "<= 0")
    markdown = (tmp_path / "out" / "report.md").read_text()
    assert "| python | impact | calls | r | c1c1c1c1c1 | a::f | a::g |" in markdown
    relaxed = edited_gates(tmp_path, "incomplete_complete_max = 0", "incomplete_complete_max = 3")
    again = verdicts(tmp_path / "again", cells, relaxed)
    assert again[("safety.incomplete_complete", "oracle cells")]["status"] == "pass"


def test_cells_that_never_counted_an_incomplete_complete_are_not_measured_not_zero(tmp_path: Path):
    lines = verdicts(tmp_path, [cell(truth=1, tp=1)])

    assert lines[("safety.incomplete_complete", "")]["status"] == "not measured"
    assert report.scip_rows([cell(truth=1, tp=1)])[0]["incomplete_complete"] is None


def test_the_silent_loss_is_counted_per_answer_with_every_missing_definition_listed_beside_what_it_shed(tmp_path: Path):
    def answer(commit: str, lost: list[str], unnamed: int) -> dict:
        return {
            "repo": "r", "language": "python", "commit": commit * 12, "mode": "impact", "missing": len(lost), "unnamed_omitted": unnamed,
            "silent": max(0, len(lost) - unnamed),
            "definitions": [{"target": t, "entries_in_file": ["a.py::other"], "entries_named_alike": []} for t in lost],
        }  # fmt: skip

    rows = [answer("a", ["a.py::f", "a.py::g", "a.py::h"], 1), answer("b", [], 4), answer("c", ["b.py::k"], 0)]
    run = run_with(tmp_path / "run", [cell(truth=1, tp=1)], silent=rows)

    assert report.main(["--run", str(run), "--out", str(tmp_path / "out")]) == 0

    found = json.loads((tmp_path / "out" / "report.json").read_text())
    silent = found["silent_loss"]
    assert (silent["answers"], silent["missing"], silent["silent"]) == (3, 4, 3)  # 2 of the first answer, 1 of the last
    assert [(x["target"], x["unnamed_omitted"], x["answer_silent"]) for x in silent["listing"]] == [
        ("a.py::f", 1, 2), ("a.py::g", 1, 2), ("a.py::h", 1, 2), ("b.py::k", 0, 1),
    ]  # fmt: skip
    line = {line["gate"]: line for line in found["verdict"]}["safety.silent_loss"]
    assert (line["status"], line["value"]) == ("read", 3)  # "read": the rows are the verdict
    markdown = (tmp_path / "out" / "report.md").read_text()
    assert "## Silent loss" in markdown and "| python | r | aaaaaaaaaa | a.py::g |" in markdown


def test_an_answer_that_lost_nothing_is_a_witnessed_zero_and_a_run_without_the_file_is_n_a(tmp_path: Path):
    clean = {
        "repo": "r",
        "language": "python",
        "commit": "c" * 12,
        "mode": "impact",
        "missing": 0,
        "unnamed_omitted": 0,
        "silent": 0,
        "definitions": [],
    }
    run = run_with(tmp_path / "run", [cell(truth=1, tp=1)], silent=[clean])

    assert report.main(["--run", str(run), "--out", str(tmp_path / "out")]) == 0

    line = {line["gate"]: line for line in json.loads((tmp_path / "out" / "report.json").read_text())["verdict"]}[
        "safety.silent_loss"
    ]
    assert (line["status"], line["value"]) == ("read", 0)
    assert verdicts(tmp_path / "bare", [cell(truth=1, tp=1)])[("safety.silent_loss", "")]["status"] == "not measured"


FACTS = Path(__file__).parent / "fixtures" / "cell_facts.json"


def paired_runs(tmp_path: Path, truth: int = 8) -> tuple[Path, Path]:
    commits = [f"c{i}" for i in range(8)]
    made = []
    for name in ("base", "head"):
        directory = tmp_path / name
        directory.mkdir()
        records = [{"repo": "r", "language": "python", "commit": c, "status": "measured"} for c in commits]
        (directory / "commits.jsonl").write_text("".join(json.dumps(x) + "\n" for x in records))
        (directory / "cells.jsonl").write_text(
            "".join(json.dumps(cell(commit=c, truth=truth, tp=truth, incomplete_complete=0)) + "\n" for c in commits)
        )
        (directory / "run.json").write_text(json.dumps({"engine": "diffctx 9", "budget": 2000}))
        made.append(directory)
    return made[0], made[1]


def test_the_cell_facts_add_the_precise_lines_and_the_cell_states_from_cellstate_and_without_them_they_stay_n_a(tmp_path: Path):
    base, head = paired_runs(tmp_path)  # 64 true relations, every one found
    out = tmp_path / "out"

    assert report.main(["--run", str(head), "--against", str(base), "--cell-facts", str(FACTS), "--out", str(out)]) == 0

    found = json.loads((out / "report.json").read_text())
    lines = {line["gate"]: line for line in found["verdict"]}
    assert {g: lines[g]["status"] for g in lines if g.startswith("precise.")} == {
        "precise.precision": "pass", "precise.recall": "pass", "precise.true_relations": "pass", "precise.decoys": "pass",
        "precise.fresh_seed": "pass", "precise.measured_cell": "fail", "precise.old_path_deleted": "pass",
    }  # fmt: skip
    assert lines["precise.true_relations"]["value"] == 64 and lines["precise.decoys"]["value"] == 12
    assert "python/imports" in lines["precise.measured_cell"]["detail"]  # declared precise, no row
    assert lines["declared.every_state_true"]["status"] == "fail"
    assert {c["relation"]: c["state"] for c in found["cells"]} == {
        "calls": "precise", "imports": "untrue", "reads": "heuristic", "dispatch": "none",
    }  # fmt: skip
    assert "## Cell states" in (out / "report.md").read_text()

    assert report.main(["--run", str(head), "--against", str(base), "--out", str(tmp_path / "bare")]) == 0
    bare = {line["gate"]: line["status"] for line in json.loads((tmp_path / "bare" / "report.json").read_text())["verdict"]}
    assert {bare[g] for g in bare if g.startswith("precise.") or g.startswith("declared.")} == {"not measured"}


def test_the_precise_lines_flip_with_gates_toml_alone_and_the_facts_need_a_paired_run(tmp_path: Path):
    base, head = paired_runs(tmp_path, truth=3)  # 24 true relations
    strict, relaxed = tmp_path / "strict", tmp_path / "relaxed"
    args = ["--run", str(head), "--against", str(base), "--cell-facts", str(FACTS)]

    assert report.main([*args, "--out", str(strict)]) == 0
    assert (
        report.main(
            [
                *args,
                "--out",
                str(relaxed),
                "--gates",
                str(edited_gates(tmp_path, "true_relations_min = 30", "true_relations_min = 20")),
            ]
        )
        == 0
    )

    def status(directory: Path) -> str:
        return {line["gate"]: line["status"] for line in json.loads((directory / "report.json").read_text())["verdict"]}[
            "precise.true_relations"
        ]

    assert (status(strict), status(relaxed)) == ("fail", "pass")
    with pytest.raises(SystemExit):
        report.main(["--run", str(head), "--cell-facts", str(FACTS), "--out", str(tmp_path / "unpaired")])


def test_the_safety_columns_replayed_from_adjudicated_rows_are_labelled_a_lower_bound(tmp_path: Path):
    run, adjudicated = tmp_path / "run", tmp_path / "adjudicated"
    run.mkdir()
    adjudicated.mkdir()
    (run / "cells.jsonl").write_text(json.dumps(cell(truth=1, tp=1)) + "\n")
    relation = {
        "repo": "r",
        "commit": "c1",
        "language": "python",
        "mode": "impact",
        "from": "a::g",
        "to": "a::f",
        "kind": "calls",
    }
    (run / "relations.jsonl").write_text(json.dumps({**relation, "verdict": "tp", "site": 1}) + "\n")
    (adjudicated / "python.jsonl").write_text(
        json.dumps({"repo": "r", "sha": "git:c1", "from": "a::g", "def": "a::f", "final": "call", "scip_kind": "call"}) + "\n"
    )

    assert report.main(["--run", str(run), "--adjudicated", str(adjudicated)]) == 0

    text = (run / "report.md").read_text()
    assert "a lower bound that undercounts the safety rows" in text
    assert json.loads((run / "report.json").read_text())["scip_adjudicated_note"] in text


def test_every_printed_incomplete_complete_says_what_complete_is_counted_on(tmp_path: Path):
    cells = [cell(truth=1, fn=1, incomplete_complete=1, reported=True)]
    pair = {"language": "python", "mode": "impact", "kind": "calls", "repo": "r", "commit": "c1", "from": "a::g", "to": "a::f"}
    run = run_with(tmp_path / "run", cells, [{**pair, "verdict": "fn", "incomplete_complete": True}])

    assert report.main(["--run", str(run), "--out", str(tmp_path / "out")]) == 0

    found = json.loads((tmp_path / "out" / "report.json").read_text())
    assert found["incomplete_complete"]["basis"] == compare.COUNTING_BASIS
    line = {line["gate"]: line for line in found["verdict"]}["safety.incomplete_complete"]
    assert compare.COUNTING_BASIS in line["detail"]
    assert "per changed definition and relation group" in (tmp_path / "out" / "report.md").read_text()


def run_marked_replayed(path: Path, cells: list[dict], silent: list[dict] = ()) -> Path:
    run = run_with(path, cells, silent=silent)
    meta = json.loads((run / "run.json").read_text())
    (run / "run.json").write_text(json.dumps({**meta, "replayed_from": "run2"}))
    return run


def test_a_verdict_on_replayed_answers_is_not_decided_whatever_the_safety_rows_say(tmp_path: Path):
    cells = [cell(truth=2, tp=2, folded=1, false_fold=1)]
    run = run_marked_replayed(tmp_path / "run", cells)

    assert report.main(["--run", str(run), "--out", str(tmp_path / "out")]) == 0

    found = json.loads((tmp_path / "out" / "report.json").read_text())
    status = {line["gate"]: line["status"] for line in found["verdict"]}
    assert status["safety.false_fold"] == "not decided: replayed answers"
    assert not [gate for gate, s in status.items() if gate.startswith("safety.") and s in ("pass", "fail")]
    markdown = (tmp_path / "out" / "report.md").read_text()
    assert "NOT DECIDED: REPLAYED ANSWERS" in markdown
    assert "Failing: safety.false_fold" not in markdown
    assert verdicts(tmp_path / "fresh", cells)[("safety.false_fold", "oracle cells")]["status"] == "fail"


def test_a_replay_marks_its_run_record_so_the_report_can_refuse_it(tmp_path: Path):
    source, out = tmp_path / "old", tmp_path / "new"
    source.mkdir()
    out.mkdir()
    (source / "run.json").write_text(json.dumps({"engine": "diffctx 9", "budget": 2000}))

    measure.mark_replayed(source, out)

    assert json.loads((out / "run.json").read_text()) == {"engine": "diffctx 9", "budget": 2000, "replayed_from": str(source)}
