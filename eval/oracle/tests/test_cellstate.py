import json
from pathlib import Path

import pytest

import cellstate
import paired
import report

FIXTURE = Path(__file__).parent / "fixtures" / "cell_facts.json"
RULE = paired.Measurability(width_max=0.20, lower_min=-0.05)
COMMITS = [f"c{i}" for i in range(8)]


def cell(commit: str, tp: int, fp: int = 0, truth: int = 10, kind: str = "calls", mode: str = "impact", fcn=False, false_fold=0):
    return {
        "language": "python", "mode": mode, "kind": kind, "repo": "r", "commit": commit, "target": "a::f", "truth": truth,
        "tp": tp, "fn": truth - tp, "fp": fp, "abstained": False, "fcn": fcn, "false_fold": false_fold,
    }  # fmt: skip


def write_run(path: Path, cells: list[dict], commits: list[str] = COMMITS) -> Path:
    path.mkdir()
    lines = [{"repo": "r", "language": "python", "commit": c, "status": "measured", "impact_error": ""} for c in commits]
    (path / "commits.jsonl").write_text("".join(json.dumps(x) + "\n" for x in lines))
    (path / "cells.jsonl").write_text("".join(json.dumps(x) + "\n" for x in cells))
    return path


def entry(tmp_path: Path, name: str, base: list[dict], head: list[dict], commits: list[str] = COMMITS) -> dict:
    result = paired.compare_runs(
        write_run(tmp_path / f"{name}-base", base, commits), write_run(tmp_path / f"{name}-head", head, commits), RULE
    )
    return {"base": f"{name}-base", "head": f"{name}-head", "budgets": (None, 2000), "result": result}


def same(**fields) -> list[dict]:
    return [cell(c, **fields) for c in COMMITS]


def state_of(entries: list[dict], facts: cellstate.Facts, relation: str = "calls", gates=None) -> cellstate.CellState:
    cells = cellstate.classify(entries, facts, report.load_gates() if gates is None else gates)
    return next(c for c in cells if (c.language, c.relation) == ("python", relation))


@pytest.fixture
def facts() -> cellstate.Facts:
    return cellstate.load_facts(FIXTURE)


def test_a_cell_meeting_every_condition_is_precise(tmp_path: Path, facts: cellstate.Facts):
    clean = entry(tmp_path, "run", same(tp=10), same(tp=10))

    found = state_of([clean], facts)

    assert (found.state, found.reasons) == ("precise", ())
    assert (found.commits, found.sites, found.head_precision, found.head_recall) == (8, 80, 1.0, 1.0)


@pytest.mark.parametrize(
    ("head", "reason"),
    [
        pytest.param(same(tp=10, fp=2), "precision 0.833 under 0.95 at impact", id="precision"),
        pytest.param(same(tp=8), "recall 0.8 under 0.9 at impact", id="recall"),
        pytest.param([cell(c, tp=10, fcn=c == "c0") for c in COMMITS], "1 false complete negatives at impact", id="fcn"),
        pytest.param([cell(c, tp=10, false_fold=1) for c in COMMITS], "8 false folds at impact", id="false fold"),
    ],
)
def test_each_missed_floor_or_safety_row_alone_makes_the_cell_heuristic_and_is_named(
    tmp_path: Path, facts: cellstate.Facts, head: list[dict], reason: str
):
    base = head  # the head is no worse than itself, so only the floor under test is missed
    found = state_of([entry(tmp_path, "run", base, head)], facts)

    assert found.state == "heuristic"
    [only] = found.reasons
    assert only.startswith(reason)


def test_a_thin_cell_is_heuristic_as_unmeasured_whatever_its_numbers(tmp_path: Path, facts: cellstate.Facts):
    few = COMMITS[:3]
    thin = entry(tmp_path, "run", [cell(c, tp=10) for c in few], [cell(c, tp=10) for c in few], few)

    found = state_of([thin], facts)

    assert (found.state, found.head_precision, found.head_recall) == ("heuristic", 1.0, 1.0)
    assert [r.split(" at ")[0] for r in found.reasons] == ["unmeasured"]


def test_a_measured_cell_that_fails_against_the_base_is_heuristic_even_above_the_floors(tmp_path: Path, facts: cellstate.Facts):
    worse = entry(tmp_path, "run", same(tp=10, truth=10), [cell(c, tp=9, truth=10) for c in COMMITS])
    gates = report.load_gates()
    assert gates["precise.recall"].limit == 0.90

    found = state_of([worse], facts)

    assert found.state == "heuristic"
    assert [r.split(" at ")[0] for r in found.reasons] == ["recall, recall_affected fail against the base"]
    assert found.head_recall == 0.9  # at the floor, so nothing but the base comparison fails it


def test_no_fresh_seed_alone_makes_the_cell_heuristic(tmp_path: Path, facts: cellstate.Facts):
    clean = entry(tmp_path, "run", same(tp=10), same(tp=10))
    stale = cellstate.Facts({}, facts.old_path_deleted)

    found = state_of([clean], stale)

    assert (found.state, found.reasons) == ("heuristic", ("no fresh seed for the language",))


def test_a_path_not_deleted_alone_makes_the_cell_heuristic(tmp_path: Path, facts: cellstate.Facts):
    clean = entry(tmp_path, "run", same(tp=10), same(tp=10))
    kept = cellstate.Facts(facts.fresh_seed, {})

    found = state_of([clean], kept)

    assert (found.state, found.reasons) == ("heuristic", ("old path not deleted",))


def test_every_mode_and_every_run_of_a_cell_must_pass(tmp_path: Path, facts: cellstate.Facts):
    good = entry(tmp_path, "a", same(tp=10), same(tp=10))
    symbol_misses = [cell(c, tp=10, mode="impact") for c in COMMITS] + [cell(c, tp=6, mode="symbol") for c in COMMITS]
    mixed = entry(tmp_path, "b", symbol_misses, symbol_misses)
    found = state_of([good, mixed], facts)

    assert found.state == "heuristic"
    assert any(reason.startswith("recall 0.6 under 0.9 at symbol (b-head, budget 2000)") for reason in found.reasons)
    assert found.head_recall == 0.6  # the weakest row


def test_a_cell_the_facts_name_but_the_table_lacks_is_heuristic_for_want_of_a_row(tmp_path: Path, facts: cellstate.Facts):
    clean = entry(tmp_path, "run", same(tp=10), same(tp=10))

    found = state_of([clean], facts, relation="imports")

    assert found.state == "heuristic"
    assert found.reasons == ("no measured row",)
    assert (found.commits, found.head_precision) == (None, None)


def test_the_floors_and_the_required_conditions_come_from_the_gates_file(tmp_path: Path, facts: cellstate.Facts):
    below = entry(tmp_path, "run", same(tp=10, fp=2), same(tp=10, fp=2))
    text = report.GATES_FILE.read_text()
    assert state_of([below], facts).state == "heuristic"

    relaxed = tmp_path / "relaxed.toml"
    relaxed.write_text(text.replace("precision_min = 0.95", "precision_min = 0.8"))
    assert state_of([below], facts, gates=report.load_gates(relaxed)).state == "precise"

    dropped = tmp_path / "dropped.toml"
    dropped.write_text(text.replace("old_path_deleted = true", "").replace("precision_min = 0.95", "precision_min = 0.8"))
    kept = cellstate.Facts(facts.fresh_seed, {})
    assert state_of([below], kept, gates=report.load_gates(dropped)).state == "precise"


def test_a_witnessless_or_malformed_facts_file_is_refused(tmp_path: Path):
    good = json.loads(FIXTURE.read_text())
    for broken in (
        {**good, "schema": "other"},
        {**good, "fresh_seed": {"python": ""}},
        {**good, "old_path_deleted": {"python": "a witness"}},
        {**good, "old_path_deleted": ["python/calls"]},
    ):
        with pytest.raises(ValueError):
            cellstate.parse_facts(broken)


def test_the_readme_table_is_generated_appended_once_and_replaced_after(tmp_path: Path, facts: cellstate.Facts):
    clean = entry(tmp_path, "run", same(tp=10), same(tp=10))
    gates = report.load_gates()
    cells = cellstate.classify([clean], facts, gates)

    table = cellstate.render_table(cells)
    assert "| python | calls | precise | 8 | 80 | 1.0 | 1.0 |  |" in table
    assert "| python | imports | heuristic | n/a | n/a | n/a | n/a | no measured row |" in table

    first = cellstate.splice("# Oracle\n\ntext\n", cellstate.render_section(cells, gates))
    assert first.count(cellstate.BEGIN) == 1 and first.startswith("# Oracle\n\ntext\n\n## Cell states")

    stale = cellstate.classify([clean], cellstate.Facts({}, facts.old_path_deleted), gates)
    second = cellstate.splice(first, cellstate.render_section(stale, gates))
    assert second.count(cellstate.BEGIN) == 1 and second.count("## Cell states") == 1
    assert "| python | calls | heuristic |" in second and "| python | calls | precise |" not in second
    assert cellstate.splice(second, cellstate.render_section(stale, gates)) == second


def test_the_command_reads_a_report_and_the_facts_and_rewrites_the_readme(tmp_path: Path, facts: cellstate.Facts):
    clean = entry(tmp_path, "run", same(tp=10), same(tp=10))
    (tmp_path / "report.json").write_text(json.dumps({"paired": [clean]}))
    readme = tmp_path / "README.md"
    readme.write_text("# Oracle\n")
    states = tmp_path / "states.json"

    code = cellstate.main(
        ["--report", str(tmp_path / "report.json"), "--facts", str(FIXTURE), "--readme", str(readme), "--json", str(states)]
    )

    assert code == 0
    assert "| python | calls | precise |" in readme.read_text()
    assert {(c["language"], c["relation"]): c["state"] for c in json.loads(states.read_text())} == {
        ("python", "calls"): "precise",
        ("python", "imports"): "heuristic",
    }
    unpaired = tmp_path / "bare.json"
    unpaired.write_text(json.dumps({"paired": []}))
    with pytest.raises(SystemExit):
        cellstate.main(["--report", str(unpaired), "--facts", str(FIXTURE)])
