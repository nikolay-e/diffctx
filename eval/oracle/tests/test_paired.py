import json
from pathlib import Path

import paired


def cell(commit: str, tp: int, truth: int, fp: int = 0, kind: str = "calls", mode: str = "impact") -> dict:
    return {
        "language": "python", "mode": mode, "kind": kind, "repo": "r", "commit": commit, "target": "a::f", "truth": truth, "tp": tp,
        "fn": truth - tp, "fp": fp, "abstained": False, "fcn": False,
    }  # fmt: skip


def detail(commit: str, source: str, verdict: str, kind: str = "calls") -> dict:
    return {"repo": "r", "commit": commit, "mode": "impact", "kind": kind, "from": source, "to": "a::f", "verdict": verdict}


def write_run(path: Path, commits: list[str], cells: list[dict], details: list[dict] = (), errors: dict | None = None) -> Path:
    path.mkdir()
    lines = [
        {"repo": "r", "language": "python", "commit": c, "status": "measured", "impact_error": (errors or {}).get(c, "")}
        for c in commits
    ]
    (path / "commits.jsonl").write_text("".join(json.dumps(x) + "\n" for x in lines))
    (path / "cells.jsonl").write_text("".join(json.dumps(x) + "\n" for x in cells))
    (path / "relations.jsonl").write_text("".join(json.dumps(x) + "\n" for x in details))
    return path


def only(result: dict) -> dict:
    [row] = result["rows"]
    return row


def test_a_loss_in_every_commit_is_a_regression_whose_interval_lies_below_zero(tmp_path: Path):
    commits = [f"c{i}" for i in range(20)]
    base = write_run(tmp_path / "base", commits, [cell(c, tp=4, truth=4) for c in commits])
    head = write_run(tmp_path / "head", commits, [cell(c, tp=3, truth=4) for c in commits])

    result = paired.compare_runs(base, head)

    recall = only(result)["recall"]
    assert (recall["base"], recall["head"], recall["delta"]) == (1.0, 0.75, -0.25)
    assert recall["high"] < 0 and recall["verdict"] == "regression"
    assert only(result)["precision"]["verdict"] == ""
    assert result["regressions"] == ["python impact calls: recall", "python impact calls: recall_affected"]


def test_a_difference_that_flips_sign_between_commits_is_not_a_regression(tmp_path: Path):
    commits = [f"c{i}" for i in range(20)]
    base = write_run(tmp_path / "base", commits, [cell(c, tp=3 + (i % 2), truth=4) for i, c in enumerate(commits)])
    head = write_run(tmp_path / "head", commits, [cell(c, tp=4 - (i % 2), truth=4) for i, c in enumerate(commits)])

    recall = only(paired.compare_runs(base, head))["recall"]

    assert recall["delta"] == 0.0
    assert recall["low"] < 0 < recall["high"] and recall["verdict"] == ""


def test_a_gain_is_marked_better_and_identical_runs_have_no_verdict(tmp_path: Path):
    commits = [f"c{i}" for i in range(12)]
    worse = write_run(tmp_path / "worse", commits, [cell(c, tp=2, truth=4, fp=2) for c in commits])
    better = write_run(tmp_path / "better", commits, [cell(c, tp=4, truth=4) for c in commits])
    twin = write_run(tmp_path / "twin", commits, [cell(c, tp=2, truth=4, fp=2) for c in commits])

    gain = only(paired.compare_runs(worse, better))
    same = only(paired.compare_runs(worse, twin))

    assert (gain["recall"]["verdict"], gain["precision"]["verdict"]) == ("better", "better")
    assert same["recall"]["verdict"] == "" and same["recall"]["low"] == same["recall"]["high"] == 0.0


def test_too_few_commits_give_no_interval_however_large_the_drop(tmp_path: Path):
    commits = ["c1", "c2"]
    base = write_run(tmp_path / "base", commits, [cell(c, tp=4, truth=4) for c in commits])
    head = write_run(tmp_path / "head", commits, [cell(c, tp=0, truth=4) for c in commits])

    row = only(paired.compare_runs(base, head))

    assert row["recall"]["delta"] == -1.0
    assert row["recall"]["low"] is None and row["recall"]["verdict"] == ""


def test_the_result_does_not_depend_on_anything_but_the_inputs(tmp_path: Path):
    commits = [f"c{i}" for i in range(15)]
    base = write_run(tmp_path / "base", commits, [cell(c, tp=3, truth=4, fp=i % 3) for i, c in enumerate(commits)])
    head = write_run(tmp_path / "head", commits, [cell(c, tp=4 - i % 2, truth=4, fp=1) for i, c in enumerate(commits)])

    assert paired.compare_runs(base, head) == paired.compare_runs(base, head)


def test_a_commit_the_engine_failed_on_leaves_the_pairing_and_is_counted_as_dropped(tmp_path: Path):
    commits = [f"c{i}" for i in range(8)]
    base = write_run(tmp_path / "base", commits, [cell(c, tp=2, truth=2) for c in commits])
    head = write_run(tmp_path / "head", commits, [cell(c, tp=2, truth=2) for c in commits[:-1]], errors={"c7": "timeout"})

    result = paired.compare_runs(base, head)

    assert result["commits"]["impact"] == 7
    assert result["dropped"] == {"base": {"impact": 1, "symbol": 0}, "head": {"impact": 0, "symbol": 0}}
    assert only(result)["recall"]["delta"] == 0.0


def test_a_relation_with_thin_truth_lists_each_pair_with_the_verdict_of_each_run(tmp_path: Path):
    commits = [f"c{i}" for i in range(6)]
    base = write_run(
        tmp_path / "base",
        commits,
        [cell(c, tp=1, truth=2) for c in commits],
        [detail("c0", "a::g", "tp"), detail("c0", "a::h", "fn"), detail("c1", "a::spurious", "fp")],
    )
    head = write_run(
        tmp_path / "head",
        commits,
        [cell(c, tp=2, truth=2) for c in commits],
        [detail("c0", "a::g", "tp"), detail("c0", "a::h", "tp")],
    )

    result = paired.compare_runs(base, head)

    assert only(result)["small"] is True and only(result)["truth"] == 12
    assert [(r["from"], r["kind"], r["base"], r["head"]) for r in result["small"]] == [
        ("a::g", "calls", "tp", "tp"),
        ("a::h", "calls", "fn", "tp"),
        ("a::spurious", "calls", "fp", "-"),
    ]


def test_a_relation_with_plenty_of_truth_is_decided_by_the_interval_alone(tmp_path: Path):
    commits = [f"c{i}" for i in range(10)]
    cells = [cell(c, tp=3, truth=3) for c in commits]
    base = write_run(tmp_path / "base", commits, cells, [detail("c0", "a::g", "tp")])
    head = write_run(tmp_path / "head", commits, cells, [detail("c0", "a::g", "tp")])

    result = paired.compare_runs(base, head)

    assert only(result)["small"] is False and result["small"] == []


def test_truth_that_differs_between_runs_is_flagged(tmp_path: Path):
    commits = [f"c{i}" for i in range(6)]
    base = write_run(tmp_path / "base", commits, [cell(c, tp=1, truth=2) for c in commits])
    head = write_run(tmp_path / "head", commits, [cell(c, tp=1, truth=3) for c in commits])

    assert only(paired.compare_runs(base, head))["truth_differs"] is True


def test_recall_over_the_affected_relations_is_bootstrapped_beside_recall_over_all(tmp_path: Path):
    commits = [f"c{i}" for i in range(20)]
    # the head lost one true positive per commit, and it was one the folding table says the change cannot affect
    base = write_run(
        tmp_path / "base", commits, [{**cell(c, tp=4, truth=4), "truth_affected": 2, "tp_affected": 2} for c in commits]
    )
    head = write_run(
        tmp_path / "head", commits, [{**cell(c, tp=3, truth=4), "truth_affected": 2, "tp_affected": 2} for c in commits]
    )

    row = only(paired.compare_runs(base, head))

    assert row["recall"]["verdict"] == "regression"
    assert (row["recall_affected"]["delta"], row["recall_affected"]["verdict"]) == (0.0, "")
    lost = write_run(
        tmp_path / "lost", commits, [{**cell(c, tp=3, truth=4), "truth_affected": 2, "tp_affected": 1} for c in commits]
    )
    assert paired.compare_runs(base, lost)["regressions"] == [
        "python impact calls: recall",
        "python impact calls: recall_affected",
    ]


def test_false_folds_of_the_head_are_summed_per_relation(tmp_path: Path):
    commits = [f"c{i}" for i in range(6)]
    base = write_run(tmp_path / "base", commits, [cell(c, tp=2, truth=2) for c in commits])
    head = write_run(
        tmp_path / "head",
        commits,
        [{**cell(c, tp=2, truth=2), "false_fold": 1} for c in commits[:3]] + [cell(c, tp=2, truth=2) for c in commits[3:]],
    )

    assert only(paired.compare_runs(base, head))["false_fold_head"] == 3
