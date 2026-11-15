import argparse
import json
import sys
from pathlib import Path

import pytest

import measure
from model import Relation


def test_a_round_fails_only_when_a_selected_language_measured_no_commit():
    assert measure.unmeasured_languages({"python": 7, "typescript": 3}) == []
    assert measure.unmeasured_languages({"python": 7, "typescript": 0}) == ["typescript"]
    assert measure.unmeasured_languages({"typescript": 0, "python": 0}) == ["python", "typescript"]
    assert measure.unmeasured_languages({}) == []


def test_a_budget_reaches_the_engine_as_a_flag_and_no_budget_adds_none():
    assert measure.impact_args("abc", budget=4000)[-2:] == ["--budget", "4000"]
    assert measure.symbol_args("f", 8000)[-2:] == ["--budget", "8000"]
    assert "--budget" not in measure.impact_args("abc")
    assert "--budget" not in measure.symbol_args("f")


def test_the_budget_option_takes_a_positive_count_or_uncapped():
    assert measure.parse_budget("2000") == 2000
    assert measure.parse_budget("uncapped") == measure.UNCAPPED_BUDGET
    for refused in ("0", "-1"):
        with pytest.raises(argparse.ArgumentTypeError):
            measure.parse_budget(refused)


def test_the_process_the_runner_starts_gets_the_budget(tmp_path: Path):
    engine = tmp_path / "engine"
    engine.write_text('#!/bin/sh\nprintf \'{"argv": "%s"}\' "$*"\n')
    engine.chmod(0o755)

    run = measure.run_diffctx(engine, tmp_path, measure.symbol_args("f", 8000), 30)

    assert run.ok and run.data is not None
    assert run.data["argv"].endswith("--symbol f -f json --budget 8000")


def test_run_meta_names_the_engine_and_budget_and_refuses_an_append_at_another_budget(tmp_path: Path):
    binary = Path(sys.executable)

    measure.write_run_meta(tmp_path, binary, 4000, append=False)
    meta = json.loads((tmp_path / measure.RUN_FILE).read_text())
    assert meta["budget"] == 4000 and meta["engine"].startswith("Python")

    measure.write_run_meta(tmp_path, binary, 4000, append=True)
    with pytest.raises(SystemExit):
        measure.write_run_meta(tmp_path, binary, 8000, append=True)


def test_a_bare_constructor_name_is_never_sampled_for_a_symbol_query():
    from changed import ChangedDefinition
    from model import Definition

    def sampled(name: str, status: str = "modified") -> ChangedDefinition:
        definition = Definition("m.ts", "method", name, ("A",), 1, 3, 0, 2, "()", name, name)
        return ChangedDefinition(status, definition, definition)

    kept = measure.symbol_candidates([sampled("constructor"), sampled("__init__"), sampled("run"), sampled("gone", "removed")])

    assert [c.head.name for c in kept] == ["run"]


def test_a_use_inside_a_changed_definition_is_scored_and_a_use_on_a_changed_module_line_is_not():
    def use(from_key: str, site: int) -> Relation:
        return Relation(from_key, "m.ts::target", "calls", site)

    relations = [use("m.ts::outer", 5), use("m.ts::outer.inner.deep", 9), use("m.ts::other", 20), use("m.ts::<module>", 30)]

    marked = measure.mark_in_diff(relations, {"m.ts::outer"}, {"m.ts": {30}})

    assert [(r.from_key, r.in_diff, r.moved) for r in marked] == [
        ("m.ts::outer", False, ("R14",)),
        ("m.ts::outer.inner.deep", False, ("R14",)),
        ("m.ts::other", False, ()),
        ("m.ts::<module>", True, ()),
    ]


def test_an_answer_kept_by_one_run_is_found_again_under_the_same_query_and_missing_ones_are_reported(tmp_path: Path):
    from corpus import load_corpus

    corpus = load_corpus()
    run = measure.RepoRun(corpus, corpus.repos[0], tmp_path, measure.Sink(tmp_path / "out"), Path("none"), False, False, 0)
    kept = measure.answer_path(tmp_path / "old", corpus.repos[0].name, "abc", "symbol-#clone")
    kept.parent.mkdir(parents=True)
    kept.write_text('{"changed": []}')

    run.replay = tmp_path / "old"
    again = measure.engine_answer(run, "abc", "symbol-#clone", tmp_path, [])
    missing = measure.engine_answer(run, "abc", "impact", tmp_path, [])

    assert (again.ok, again.data) == (True, {"changed": []})
    assert (missing.ok, missing.error) == (False, "not stored")
    assert kept.name == "abc-symbol-_clone.json"


def test_a_use_by_the_definition_that_holds_a_changed_target_is_not_asked_of_the_engine():
    inner = Relation("m.py::test.run", "m.py::test.run.cmd", "passes_as_value", 6)
    outer = Relation("m.py::test", "m.py::test.run.cmd", "passes_as_value", 7)
    sibling = Relation("m.py::other", "m.py::test.run.cmd", "calls", 9)

    marked = measure.mark_in_diff([inner, outer, sibling], {"m.py::test", "m.py::test.run"}, {})

    assert [(r.from_key, r.in_diff) for r in marked] == [("m.py::test.run", True), ("m.py::test", True), ("m.py::other", False)]


def test_extra_engine_arguments_reach_the_engine_and_the_run_record(tmp_path: Path):
    assert measure.impact_args("abc", extra=("--expand-folded",))[-1] == "--expand-folded"
    assert measure.symbol_args("f", 2000, ("--expand-folded",))[-3:] == ["--budget", "2000", "--expand-folded"]

    measure.write_run_meta(tmp_path, Path(sys.executable), 2000, append=False, engine_args=("--expand-folded",))

    assert json.loads((tmp_path / measure.RUN_FILE).read_text())["engine_args"] == ["--expand-folded"]
    with pytest.raises(SystemExit):
        measure.write_run_meta(tmp_path, Path(sys.executable), 2000, append=True, engine_args=())


def test_a_replay_scores_the_repositories_the_replayed_run_answered(tmp_path: Path):
    rows = [
        {"repo": "click", "commit": "a", "status": "measured"},
        {"repo": "hono", "commit": "b", "status": "measured"},
        {"repo": "fd", "commit": "c", "status": "excluded"},
        {"repo": "click", "commit": "d", "status": "measured"},
    ]
    (tmp_path / "commits.jsonl").write_text("".join(json.dumps(r) + "\n" for r in rows))

    assert measure.replayed_repositories(tmp_path) == ["click", "hono"]


def test_the_users_of_an_added_definition_inside_the_range_are_not_scored_and_those_of_an_edited_one_are():
    users = [Relation("m.py::caller", "m.py::target", "calls", 5), Relation("m.py::outside", "m.py::target", "calls", 9)]

    edited = measure.mark_in_diff(users, {"m.py::caller"}, {})
    added = measure.mark_in_diff(users, {"m.py::caller"}, {}, added=True)

    assert [(r.from_key, r.in_diff, r.moved) for r in edited] == [("m.py::caller", False, ("R14",)), ("m.py::outside", False, ())]
    assert [(r.from_key, r.in_diff) for r in added] == [("m.py::caller", True), ("m.py::outside", False)]


def test_a_replay_serves_stored_answers_and_asks_the_given_engine_for_the_rest(tmp_path: Path):
    from corpus import load_corpus

    corpus = load_corpus()
    engine = tmp_path / "engine"
    engine.write_text("#!/bin/sh\nprintf '{\"asked\": true}'\n")
    engine.chmod(0o755)
    old = tmp_path / "old"
    kept = measure.answer_path(old, corpus.repos[0].name, "abc", "impact")
    kept.parent.mkdir(parents=True)
    kept.write_text('{"asked": false}')
    run = measure.RepoRun(corpus, corpus.repos[0], tmp_path, measure.Sink(tmp_path / "out"), engine, False, False, 0, None, old)

    stored = measure.engine_answer(run, "abc", "impact", tmp_path, [])
    asked = measure.engine_answer(run, "abc", "symbol-f", tmp_path, ["--symbol", "f"])
    run.binary = Path("replay")
    missing = measure.engine_answer(run, "abc", "symbol-g", tmp_path, [])

    assert stored.data == {"asked": False} and asked.data == {"asked": True}
    assert (missing.ok, missing.error) == (False, "not stored")
    assert measure.answer_path(tmp_path / "out", corpus.repos[0].name, "abc", "impact").exists()  # the new run is whole
    assert measure.answer_path(tmp_path / "out", corpus.repos[0].name, "abc", "symbol-f").exists()


def test_unfold_is_passed_to_an_engine_that_knows_it_and_only_once(tmp_path: Path):
    knows = tmp_path / "knows"
    knows.write_text('#!/bin/sh\necho "      --unfold   list every row"\n')
    knows.chmod(0o755)
    older = tmp_path / "older"
    older.write_text('#!/bin/sh\necho "      --budget N"\n')
    older.chmod(0o755)

    assert measure.engine_arguments(knows, (), folded=False) == ("--unfold",)
    assert measure.engine_arguments(knows, ("--unfold",), folded=False) == ("--unfold",)
    assert measure.engine_arguments(knows, ("--x",), folded=False) == ("--x", "--unfold")
    assert measure.engine_arguments(knows, (), folded=True) == ()
    assert measure.engine_arguments(older, (), folded=False) == ()
    assert measure.engine_arguments(tmp_path / "missing", (), folded=False) == ()


def test_a_scored_impact_answer_writes_one_silent_loss_row_with_every_missing_definition(tmp_path: Path):
    from answer import Answer
    from compare import missing_definition
    from model import Definition

    assert "silent_loss.jsonl" in measure.RESULT_FILES
    lost = Definition("m.py", "function", "lost", (), 1, 3, 0, 2, "()", "h", "s")
    other = Definition("m.py", "function", "other", (), 5, 7, 4, 2, "()", "h", "s")
    answer = Answer({}, [], set(), False, unnamed_omitted=1)
    sink = measure.Sink(tmp_path)

    measure.write_silent_loss(
        sink, {"repo": "r", "commit": "c1", "mode": "impact"}, [missing_definition(d, answer) for d in (lost, other)], answer
    )
    measure.write_silent_loss(sink, {"repo": "r", "commit": "c2", "mode": "impact"}, [], answer)

    first, second = (json.loads(line) for line in (tmp_path / "silent_loss.jsonl").read_text().splitlines())
    assert (first["commit"], first["missing"], first["unnamed_omitted"], first["silent"]) == ("c1", 2, 1, 1)
    assert [d["target"] for d in first["definitions"]] == [lost.key, other.key]
    assert (second["missing"], second["silent"], second["definitions"]) == (0, 0, [])  # a witnessed zero
