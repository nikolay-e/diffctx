import json
import os
from pathlib import Path

import pytest

import blind
import lane

SHA = "c0ffee" * 6 + "abcd"
SECRETS = (SHA, "secret_target", "src/secret_file.py", "secret_caller")
STUB = f"""
import json, sys
from pathlib import Path

args = sys.argv[1:]
repo, out = args[args.index("--repo") + 1], Path(args[args.index("--out") + 1])
budget = (200000 if args[args.index("--budget") + 1] == "uncapped" else int(args[args.index("--budget") + 1])) if "--budget" in args else None
out.mkdir(parents=True, exist_ok=True)

def rows(name, items):
    with (out / name).open("a") as handle:
        handle.writelines(json.dumps(item) + "\\n" for item in items)

(out / "run.json").write_text(json.dumps({{"engine": "diffctx 9", "budget": budget, "fresh": True}}))
cell = {{"repo": repo, "language": "python", "mode": "impact", "kind": "calls", "commit": "{SHA}",
        "target": "secret_target", "incomplete_complete": {{INCOMPLETE}}, "false_fold": 0}}
rows("cells.jsonl", [cell, {{**cell, "mode": "symbol", "incomplete_complete": 0}}])
rows("silent_loss.jsonl", [{{"repo": repo, "language": "python", "mode": "impact", "commit": "{SHA}", "missing": 1,
        "unnamed_omitted": 0, "silent": 1, "definitions": [{{"target": "secret_target", "file": "src/secret_file.py"}}]}}])
rows("commits.jsonl", [{{"repo": repo, "language": "python", "commit": "{SHA}", "status": "measured", "reason": "secret_caller"}}])
rows("indexes.jsonl", [{{"repo": repo, "commit": "{SHA}", "side": "head", "ok": True, "reason": "src/secret_file.py"}}] * 2)
print("[" + repo + " 1/1] {SHA} measured secret_caller", flush=True)
print("secret_target src/secret_file.py", file=sys.stderr)
"""


def stub(tmp_path: Path, incomplete: int) -> Path:
    script = tmp_path / "measure_stub.py"
    script.write_text(STUB.replace("{INCOMPLETE}", str(incomplete)))
    return script


def blind_round(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, incomplete: int = 0, repos=("a",)) -> Path:
    monkeypatch.setattr(lane, "CACHE", tmp_path / "cache")
    counters = tmp_path / "counters"
    lane.run_lane("python", list(repos), tmp_path / "out", dict(os.environ), stub(tmp_path, incomplete), counters=counters)
    return counters


def test_a_blind_lane_prints_no_row_and_keeps_only_counters_outside_the_rows(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capfd: pytest.CaptureFixture[str]
):
    counters = blind_round(tmp_path, monkeypatch)

    shown = capfd.readouterr()
    assert not [secret for secret in SECRETS if secret in shown.out + shown.err]
    assert "=== measure default a ===" in shown.out
    assert sorted(f.name for f in counters.iterdir()) == ["default-python.json", "uncapped-python.json"]
    text = "".join(f.read_text() for f in counters.iterdir())
    assert not [secret for secret in SECRETS if secret in text]
    assert (tmp_path / "out" / "default" / "python" / "cells.jsonl").exists()  # the rows stay beside the log, to be dropped
    assert SHA in (tmp_path / "out" / "default" / "a.log").read_text()


def test_a_round_that_is_not_blind_prints_what_the_measure_prints(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capfd):
    monkeypatch.setattr(lane, "CACHE", tmp_path / "cache")

    lane.run_lane("python", ["a"], tmp_path / "out", dict(os.environ), stub(tmp_path, 0))

    assert SHA in capfd.readouterr().out


def test_the_counters_are_sums_per_language_and_mode_and_counts_per_repository(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    counters = blind.counters(blind_run(tmp_path, monkeypatch, incomplete=2))

    assert counters["schema"] == blind.SCHEMA and counters["budget"] is None and counters["fresh"]
    impact, symbol = counters["groups"]
    assert (impact["mode"], impact["cells"], impact["incomplete_complete"], impact["false_fold"]) == ("impact", 1, 2, 0)
    assert (impact["answers"], impact["missing"], impact["silent"]) == (1, 1, 1)
    assert (symbol["mode"], symbol["cells"], symbol["incomplete_complete"], symbol.get("answers", 0)) == ("symbol", 1, 0, 0)
    assert counters["repos"] == [
        {"repo": "a", "language": "python", "snapshots": 2, "indexed_ok": 2, "commits": 1, "measured": 1}
    ]


def blind_run(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, incomplete: int) -> Path:
    blind_round(tmp_path, monkeypatch, incomplete)
    return tmp_path / "out" / "default" / "python"


def gate(tmp_path: Path, counters: Path, capsys) -> tuple[int, dict, str]:
    status = blind.main(["--out", str(tmp_path / "report"), "--gate", str(counters)])
    return (
        status,
        json.loads((tmp_path / "report" / "blind.json").read_text()),
        (tmp_path / "report" / "blind.md").read_text(),
    )


def test_a_report_built_from_rows_prints_no_row_and_names_the_counters_by_budget(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys
):
    counters = blind_round(tmp_path, monkeypatch, incomplete=0, repos=("a", "b"))

    status, document, markdown = gate(tmp_path, counters, capsys)

    assert status == 0, capsys.readouterr().err
    assert not [secret for secret in SECRETS if secret in markdown + json.dumps(document)]
    assert sorted(document["budgets"]) == ["default", "uncapped"]
    impact = next(g for g in document["budgets"]["default"]["groups"] if g["mode"] == "impact")
    assert (impact["cells"], impact["incomplete_complete"], impact["answers"], impact["silent"]) == (
        2,
        0,
        2,
        2,
    )  # two repositories
    assert "## Budget: default" in markdown and "safety.false_fold" in markdown and "safety.silent_loss" in markdown


def test_one_incomplete_complete_fails_the_gate_and_is_a_counter_not_a_pair(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys
):
    counters = blind_round(tmp_path, monkeypatch, incomplete=1)

    status, document, markdown = gate(tmp_path, counters, capsys)

    failing = [line for line in document["budgets"]["default"]["verdict"] if line["status"] == "fail"]
    assert status == 1 and [line["gate"] for line in failing] == ["safety.incomplete_complete"]
    assert failing[0]["value"] == 1 and "not listed" in failing[0]["detail"]
    assert "Failing: safety.incomplete_complete" in markdown


def test_a_repository_below_the_coverage_floor_stops_the_gate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys):
    counters = blind_round(tmp_path, monkeypatch)
    for file in counters.iterdir():
        part = json.loads(file.read_text())
        part["repos"][0].update(snapshots=10, indexed_ok=1)
        file.write_text(json.dumps(part))

    status, document, _ = gate(tmp_path, counters, capsys)

    assert status == 1
    assert any(
        line["gate"] == "coverage.indexed_share" and line["status"] != "pass"
        for line in document["budgets"]["default"]["verdict"]
    )


def test_parts_of_two_engines_or_without_the_fresh_mark_decide_nothing(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    counters = blind_round(tmp_path, monkeypatch)
    part = json.loads((counters / "default-python.json").read_text())

    with pytest.raises(SystemExit, match="more than one engine"):
        blind.join([part, {**part, "engine": "diffctx 8"}])
    with pytest.raises(SystemExit, match="fresh"):
        blind.join([{**part, "fresh": False}])


def test_an_index_only_lane_asks_the_index_round_for_the_repositorys_whole_quota_and_measures_nothing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    from corpus import load_corpus

    monkeypatch.setattr(lane, "CACHE", tmp_path / "cache")
    script = tmp_path / "index_stub.py"
    script.write_text("import os, sys, json\nopen(os.environ['STUB_LOG'], 'a').write(json.dumps(sys.argv[1:]) + '\\n')\n")
    corpus = load_corpus()
    repo = next(r for r in corpus.repos if r.language == "go")

    status = lane.index_lane([repo.name], {**os.environ, "STUB_LOG": str(tmp_path / "calls.jsonl")}, script)

    (call,) = [json.loads(line) for line in (tmp_path / "calls.jsonl").read_text().splitlines()]
    assert status == 0
    assert call[call.index("--repo") + 1] == repo.name and call[call.index("--commits") + 1] == str(corpus.quota(repo))
    assert {"--no-setup", "--retry-failed", "--drop-trees"} <= set(call) and "--out" not in call
    assert not (tmp_path / "out").exists()
