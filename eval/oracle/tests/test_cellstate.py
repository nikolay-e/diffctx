import json
from pathlib import Path

import pytest

import cellstate
import paired
from gates import GATES_FILE, load_gates

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
    cells = cellstate.classify(entries, facts, load_gates() if gates is None else gates)
    return next(c for c in cells if (c.language, c.relation) == ("python", relation))


def declare(state: str, **fields) -> cellstate.Facts:
    """The fixture's engine with `python/calls` declared `state`, and its fields where they differ from the fixture's."""
    loaded = cellstate.load_facts(FIXTURE)
    shown = {"old_path_deleted": "a commit", "decoys": 14, "unknown": "dispatch is not traced" if state == "none" else None}
    return cellstate.Facts(
        loaded.fresh_seed, {**loaded.cells, ("python", "calls"): cellstate.CellFacts(state, **{**shown, **fields})}
    )


@pytest.fixture
def facts() -> cellstate.Facts:
    return cellstate.load_facts(FIXTURE)


def test_a_cell_declared_precise_that_meets_every_condition_is_precise(tmp_path: Path, facts: cellstate.Facts):
    clean = entry(tmp_path, "run", same(tp=10), same(tp=10))

    found = state_of([clean], facts)

    assert (found.declared, found.state, found.reasons, found.violations) == ("precise", "precise", (), ())
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
def test_each_missed_floor_or_safety_row_alone_makes_a_precise_declaration_untrue_and_is_named(
    tmp_path: Path, facts: cellstate.Facts, head: list[dict], reason: str
):
    base = head  # the head is no worse than itself, so only the floor under test is missed
    found = state_of([entry(tmp_path, "run", base, head)], facts)

    assert (found.declared, found.state) == ("precise", "untrue")
    [only] = found.violations
    assert only.startswith(reason)
    assert found.reasons == found.violations


def test_a_thin_cell_is_untrue_as_unmeasured_whatever_its_numbers(tmp_path: Path, facts: cellstate.Facts):
    few = COMMITS[:3]
    thin = entry(tmp_path, "run", [cell(c, tp=10) for c in few], [cell(c, tp=10) for c in few], few)

    found = state_of([thin], facts)

    assert (found.state, found.head_precision, found.head_recall) == ("untrue", 1.0, 1.0)
    assert [r.split(" at ")[0] for r in found.reasons] == ["unmeasured"]


def test_a_measured_cell_that_fails_against_the_base_is_untrue_even_above_the_floors(tmp_path: Path, facts: cellstate.Facts):
    worse = entry(tmp_path, "run", same(tp=10, truth=10), [cell(c, tp=9, truth=10) for c in COMMITS])
    assert load_gates()["precise.recall"].limit == 0.90

    found = state_of([worse], facts)

    assert found.state == "untrue"
    assert [r.split(" at ")[0] for r in found.reasons] == ["recall, recall_affected fail against the base"]
    assert found.head_recall == 0.9  # at the floor, so nothing but the base comparison fails it


def test_no_fresh_seed_alone_makes_a_precise_declaration_untrue(tmp_path: Path, facts: cellstate.Facts):
    clean = entry(tmp_path, "run", same(tp=10), same(tp=10))

    found = state_of([clean], cellstate.Facts({}, facts.cells))

    assert (found.state, found.violations) == ("untrue", ("no fresh seed for the language",))


def test_a_path_not_deleted_alone_makes_a_precise_declaration_untrue(tmp_path: Path, facts: cellstate.Facts):
    clean = entry(tmp_path, "run", same(tp=10), same(tp=10))

    found = state_of([clean], declare("precise", old_path_deleted=None))

    assert (found.state, found.violations) == ("untrue", ("old path not deleted",))


@pytest.mark.parametrize(
    ("decoys", "reason"),
    [(9, "9 decoys, under 10"), (None, "no decoy count in the facts")],
)
def test_too_few_decoys_alone_make_a_precise_declaration_untrue(tmp_path: Path, decoys, reason: str):
    clean = entry(tmp_path, "run", same(tp=10), same(tp=10))

    found = state_of([clean], declare("precise", decoys=decoys))

    assert (found.state, found.violations) == ("untrue", (reason,))
    assert state_of([clean], declare("precise", decoys=10)).state == "precise"  # the floor is inclusive


def test_a_cell_needs_thirty_true_relations_in_every_paired_run_summed_over_its_modes(tmp_path: Path, facts: cellstate.Facts):
    def run(name: str, impact: int, symbol: int) -> dict:
        rows = [cell(c, tp=impact, truth=impact) for c in COMMITS] + [
            cell(c, tp=symbol, truth=symbol, mode="symbol") for c in COMMITS[:5]
        ]
        return entry(tmp_path, name, rows, rows)

    # 4 impact sites per commit over 8 commits and 2 symbol sites over 5: 42 true relations, though no mode has 30 apart
    enough = state_of([run("one", 4, 2)], facts)
    assert (enough.state, enough.values["precise.true_relations"]) == ("precise", 42)

    thin = state_of([run("a", 4, 2), run("b", 3, 1)], facts)  # the second run holds 24 + 5
    assert thin.state == "untrue" and thin.violations == ("29 true relations, under 30",)
    assert thin.values["precise.true_relations"] == 29


def test_every_mode_and_every_run_of_a_cell_must_pass(tmp_path: Path, facts: cellstate.Facts):
    good = entry(tmp_path, "a", same(tp=10), same(tp=10))
    symbol_misses = [cell(c, tp=10, mode="impact") for c in COMMITS] + [cell(c, tp=6, mode="symbol") for c in COMMITS]
    mixed = entry(tmp_path, "b", symbol_misses, symbol_misses)
    found = state_of([good, mixed], facts)

    assert found.state == "untrue"
    assert any(reason.startswith("recall 0.6 under 0.9 at symbol (b-head, budget 2000)") for reason in found.reasons)
    assert found.head_recall == 0.6  # the weakest row


def test_a_cell_declared_precise_that_the_table_lacks_is_untrue_for_want_of_a_row(tmp_path: Path, facts: cellstate.Facts):
    clean = entry(tmp_path, "run", same(tp=10), same(tp=10))

    found = state_of([clean], facts, relation="imports")

    assert (found.declared, found.state) == ("precise", "untrue")
    assert found.violations == ("no measured row",)
    assert (found.commits, found.head_precision) == (None, None)


def test_a_cell_declared_heuristic_answers_as_candidates_so_only_a_safety_row_makes_it_untrue(tmp_path: Path):
    weak = same(tp=8, fp=2)  # well under the precise floors, which a heuristic cell does not claim
    shaky = entry(tmp_path, "run", weak, weak)

    found = state_of([shaky], declare("heuristic", old_path_deleted=None, decoys=None))

    assert (found.declared, found.state, found.violations) == ("heuristic", "heuristic", ())
    assert any(reason.startswith("precision 0.8 under 0.95") for reason in found.reasons)  # why it is not precise
    wrong = [cell(c, tp=8, fp=2, fcn=c == "c0") for c in COMMITS]
    claimed = state_of([entry(tmp_path, "wrong", wrong, wrong)], declare("heuristic"))
    assert (claimed.state, claimed.violations) == (
        "untrue",
        ("1 false complete negatives at impact (wrong-head, budget 2000), limit 0",),
    )


def test_a_cell_declared_none_is_a_named_unknown_never_a_zero_and_claims_no_complete_answer(
    tmp_path: Path, facts: cellstate.Facts
):
    silent = entry(
        tmp_path,
        "run",
        same(tp=0),
        same(tp=0),
    )
    guessed = [cell(c, tp=0, fcn=c == "c0") for c in COMMITS]

    found = state_of([silent], facts, relation="dispatch")
    assert (found.declared, found.state, found.violations) == ("none", "none", ())
    assert found.reasons == ("unknown: dynamic dispatch through a registry is not traced",)
    assert (found.commits, found.sites, found.head_precision, found.head_recall) == (None, None, None, None)

    measured = state_of([entry(tmp_path, "wrong", guessed, guessed)], declare("none"))
    assert (measured.state, measured.head_recall) == ("untrue", None)  # it claimed an absence it said it did not know
    assert measured.violations[0].startswith("1 false complete negatives")


def test_a_measured_cell_no_declaration_names_is_untrue(tmp_path: Path, facts: cellstate.Facts):
    clean = entry(tmp_path, "run", same(tp=10), same(tp=10))
    cells = cellstate.classify([clean], cellstate.Facts(facts.fresh_seed, {}), load_gates())

    [only] = cells
    assert (only.declared, only.state, only.violations) == (
        None,
        "untrue",
        ("the engine declares no state for this measured cell",),
    )


def test_the_floors_and_the_required_conditions_come_from_the_gates_file(tmp_path: Path, facts: cellstate.Facts):
    below = entry(tmp_path, "run", same(tp=10, fp=2), same(tp=10, fp=2))
    text = GATES_FILE.read_text()
    assert state_of([below], facts).state == "untrue"

    relaxed = tmp_path / "relaxed.toml"
    relaxed.write_text(text.replace("precision_min = 0.95", "precision_min = 0.8"))
    assert state_of([below], facts, gates=load_gates(relaxed)).state == "precise"

    dropped = tmp_path / "dropped.toml"
    dropped.write_text(text.replace("old_path_deleted = true", "").replace("precision_min = 0.95", "precision_min = 0.8"))
    kept = declare("precise", old_path_deleted=None)
    assert state_of([below], kept, gates=load_gates(dropped)).state == "precise"


def test_the_minimum_of_true_relations_and_decoys_comes_from_the_gates_file_as_the_w7d_ruling_has_it(
    tmp_path: Path, facts: cellstate.Facts
):
    gates = load_gates()
    assert (gates["precise.true_relations"].limit, gates["precise.decoys"].limit) == (30, 10)
    tiny = entry(tmp_path, "run", same(tp=3, truth=3), same(tp=3, truth=3))  # 24 sites
    assert state_of([tiny], facts).violations == ("24 true relations, under 30",)

    relaxed = tmp_path / "relaxed.toml"
    relaxed.write_text(GATES_FILE.read_text().replace("true_relations_min = 30", "true_relations_min = 20"))
    assert state_of([tiny], facts, gates=load_gates(relaxed)).state == "precise"


def test_a_witnessless_or_malformed_facts_file_is_refused():
    good = json.loads(FIXTURE.read_text())
    calls = good["cells"]["python/calls"]
    for broken in (
        {**good, "schema": "diffctx.cell-facts.v1"},
        {**good, "fresh_seed": {"python": ""}},
        {**good, "cells": {"python": calls}},
        {**good, "cells": {"python/calls": {**calls, "state": "exact"}}},
        {**good, "cells": {"python/calls": {**calls, "old_path_deleted": ""}}},
        {**good, "cells": {"python/calls": {**calls, "decoys": -1}}},
        {**good, "cells": {"python/dispatch": {"state": "none"}}},  # a `none` that names nothing unknown
        {**good, "cells": ["python/calls"]},
    ):
        with pytest.raises(ValueError):
            cellstate.parse_facts(broken)


def lines(cells: list[cellstate.CellState]) -> dict[str, tuple]:
    seen = cellstate.observations(cells, load_gates())
    return {gate: found[0][1:] for gate, found in seen.items()}


def test_the_precise_lines_are_the_weakest_value_over_the_cells_declared_precise_and_say_who_fails(
    tmp_path: Path, facts: cellstate.Facts
):
    good = entry(tmp_path, "run", same(tp=10), same(tp=10))
    rows = [cell(c, tp=8, fp=2) for c in COMMITS] + [cell(c, tp=10, kind="imports") for c in COMMITS]
    cells = cellstate.classify([entry(tmp_path, "both", rows, rows)], facts, load_gates())
    shown = lines(cells)

    assert shown["precise.precision"][0] == 0.8
    assert shown["precise.decoys"][0] == 12 and "failing" not in shown["precise.decoys"][1]
    assert shown["precise.fresh_seed"][0] is True
    assert "failing: python/calls" in shown["precise.precision"][1]
    assert shown["declared.every_state_true"][0] is False
    assert "untrue: python/calls" in shown["declared.every_state_true"][1]
    assert lines(cellstate.classify([good], facts, load_gates()))["declared.every_state_true"][0] is False  # imports has no row


def test_with_no_cell_declared_precise_there_is_no_precise_line_to_judge(tmp_path: Path):
    only_candidates = cellstate.Facts({}, {("python", "calls"): cellstate.CellFacts("heuristic")})
    cells = cellstate.classify([entry(tmp_path, "run", same(tp=10), same(tp=10))], only_candidates, load_gates())

    shown = lines(cells)

    assert not [gate for gate in shown if gate.startswith("precise.")]
    assert shown["declared.every_state_true"][0] is True


def test_the_readme_table_is_generated_appended_once_and_replaced_after(tmp_path: Path, facts: cellstate.Facts):
    clean = entry(tmp_path, "run", same(tp=10), same(tp=10))
    gates = load_gates()
    cells = cellstate.classify([clean], facts, gates)

    table = cellstate.render_table(cells)
    assert "| python | calls | precise | precise | 8 | 80 | 1.0 | 1.0 |  |" in table
    assert "| python | imports | precise | untrue | n/a | n/a | n/a | n/a | no measured row |" in table
    assert (
        "| python | dispatch | none | none | n/a | n/a | n/a | n/a | unknown: dynamic dispatch through a registry is not traced |"
        in table
    )

    first = cellstate.splice("# Oracle\n\ntext\n", cellstate.render_section(cells, gates))
    assert first.count(cellstate.BEGIN) == 1 and first.startswith("# Oracle\n\ntext\n\n## Cell states")

    stale = cellstate.classify([clean], cellstate.Facts({}, facts.cells), gates)
    second = cellstate.splice(first, cellstate.render_section(stale, gates))
    assert second.count(cellstate.BEGIN) == 1 and second.count("## Cell states") == 1
    assert "| python | calls | precise | untrue |" in second and "| python | calls | precise | precise |" not in second
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
    assert "| python | calls | precise | precise |" in readme.read_text()
    assert {(c["language"], c["relation"]): c["state"] for c in json.loads(states.read_text())} == {
        ("python", "calls"): "precise",
        ("python", "imports"): "untrue",
        ("python", "reads"): "heuristic",
        ("python", "dispatch"): "none",
    }
    unpaired = tmp_path / "bare.json"
    unpaired.write_text(json.dumps({"paired": []}))
    with pytest.raises(SystemExit):
        cellstate.main(["--report", str(unpaired), "--facts", str(FIXTURE)])
