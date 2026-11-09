import dataclasses
import json
import re
from pathlib import Path

import pytest

import answer
from answer import V1, V2, parse_answer
from compare import KIND_PRIORITY

FIXTURES = json.loads((Path(__file__).parent / "fixtures" / "v1_answers.json").read_text())


class Resolve:
    def __call__(self, path, symbol, lines):
        return f"{path}::{symbol}" if symbol else f"{path}::<module>"

    def by_span(self, path, lines):
        return f"{path}::span:{lines}"


def normal(value):
    if isinstance(value, dict):
        return {k: normal(v) for k, v in sorted(value.items())}
    if isinstance(value, (list, tuple)):
        return [normal(x) for x in value]
    if isinstance(value, set):
        return sorted(value)
    return value


@pytest.mark.parametrize("case", FIXTURES, ids=[f"{c['engine']}-{i}" for i, c in enumerate(FIXTURES)])
def test_a_stored_v1_answer_is_read_exactly_as_it_was_before_v2_existed(case):
    # `expected` was written by the reader as it stood at 79b5df6a, before the schemas were split into adapters.
    assert normal(dataclasses.asdict(parse_answer(case["answer"], Resolve()))) == case["expected"]


def test_the_fixtures_cover_every_feature_of_a_v1_answer_the_reader_understands():
    seen = set()
    for case in FIXTURES:
        a = case["answer"]
        entries = a.get("changed", [])
        for key in ("entry_unlisted", "layout_only", "unanalysed_files", "truncated"):
            if a.get(key):
                seen.add(key)
        for entry in entries:
            seen |= {
                key
                for key in ("unresolved", "callers_omitted", "omitted", "also_changed", "entry", "folded", "missed_in")
                if entry.get(key)
            }
            seen |= {"folded_by"} if any(c.get("folded_by") for c in entry.get("callers", [])) else set()
    assert {
        "unresolved",
        "callers_omitted",
        "omitted",
        "also_changed",
        "entry",
        "folded",
        "folded_by",
        "missed_in",
        "truncated",
        "entry_unlisted",
        "layout_only",
        "unanalysed_files",
    } <= seen


def test_an_answer_names_its_schema_and_one_that_names_none_is_v1():
    entry = {"path": "a.py", "symbol": "f", "lines": "1-2", "callers": [{"path": "b.py", "symbol": "g", "lines": "1-2"}]}

    older = parse_answer({"changed": [entry]}, Resolve())
    stated = parse_answer({"schema": V1, "changed": [entry]}, Resolve())

    assert older == stated and [c.from_key for c in older.targets["a.py::f"].callers] == ["b.py::g"]
    with pytest.raises(ValueError, match=r"diffctx\.impact\.v3"):
        parse_answer({"schema": "diffctx.impact.v3"}, Resolve())


def test_the_kinds_a_row_may_state_are_the_kinds_the_scorer_ranks():
    assert set(answer.ROW_KINDS) == set(KIND_PRIORITY)


# --- v2 ---------------------------------------------------------------------------------------------------------------


def v2(**fields):
    base = {
        "schema": V2,
        "definitions": [],
        "empty": False,
        "summary": {"shed": 0},
        "coverage": {"scope": {"complete": True}, "limits": [], "unanalysed": {"total": 0, "items": []}},
        "unlisted": {},
    }
    return {**base, **fields}


def definition(**fields):
    base = {
        "path": "a.py",
        "symbol": "f",
        "lines": "1-3",
        "change": {"kind": "edited"},
        "rows": [],
        "coverage": {"calls": {"complete": True}, "reads": {"complete": True}},
        "shed": {},
    }
    return {**base, **fields}


def row(**fields):
    return {
        "path": "b.py",
        "symbol": "g",
        "lines": "5-9",
        "class": "call",
        "standing": "resolved",
        "guard": {"state": "none"},
        **fields,
    }


def test_a_v2_row_is_read_by_its_class_standing_kind_and_fold():
    doc = v2(
        definitions=[
            definition(
                rows=[
                    row(),
                    row(symbol="h", **{"class": "reference", "standing": "candidate", "kind": "reads"}),
                    row(symbol="i", **{"class": "reexports"}),
                    row(symbol="j", **{"class": "implementation", "folded_by": "no-code-change", "kind": "not-a-kind"}),
                ]
            )
        ]
    )

    callers = parse_answer(doc, Resolve()).targets["a.py::f"].callers

    assert [(c.from_key, c.evidence, c.relation, c.kind, c.folded_by) for c in callers] == [
        ("b.py::g", "resolved", "call", None, None),
        ("b.py::h", "candidate", "reference", "reads", None),
        ("b.py::i", "resolved", "reexports", None, None),
        ("b.py::j", "resolved", "call", None, "no-code-change"),
    ]


def test_a_v2_gap_in_any_search_makes_the_target_a_partial_answer_and_none_leaves_it_whole():
    whole = parse_answer(v2(definitions=[definition()]), Resolve()).targets["a.py::f"]
    unlanded = definition(
        gaps=[{"kind": "unlanded", "uses": 3}],
        coverage={"calls": {"complete": True}, "reads": {"complete": False, "gaps": [{"kind": "unlanded", "uses": 3}]}},
    )
    unread = definition(
        coverage={"calls": {"complete": False, "gaps": [{"kind": "unread", "files": 2}]}, "reads": {"complete": True}}
    )

    partial = parse_answer(v2(definitions=[unlanded]), Resolve()).targets["a.py::f"]
    missed = parse_answer(v2(definitions=[unread]), Resolve()).targets["a.py::f"]

    assert not whole.unresolved and not whole.missed_in
    assert partial.unresolved == "unlanded" and missed.unresolved == "unread" and missed.missed_in == ["2 unread file(s)"]


def test_what_a_v2_definition_shed_is_its_ledger_total_and_one_the_ledger_attributes_is_no_unattributed_truncation():
    cut = definition(shed={"resolved": 3, "candidate": 1, "name": 1, "untested": 2})

    answer_ = parse_answer(v2(definitions=[cut], summary={"shed": 5}), Resolve())

    assert answer_.targets["a.py::f"].callers_omitted == 5
    assert (answer_.truncated, answer_.truncated_unattributed) == (5, 0)
    unowned = parse_answer(v2(definitions=[definition()], summary={"shed": 4}), Resolve())
    assert (unowned.truncated, unowned.truncated_unattributed) == (4, 4)


def test_v2_names_what_it_shed_whole_and_how_many_more_it_names_nowhere():
    omitted = {
        "definitions": {
            "total": 5,
            "items": [
                {"def": {"path": "c.py", "symbol": "h", "lines": "1-9"}, "shed": {"resolved": 2}},
                {"def": {"path": "d.py", "symbol": "k", "lines": "4-8"}, "shed": {"candidate": 1}},
            ],
        },
        "needs": 900,
    }

    parsed = parse_answer(v2(omitted=omitted), Resolve())

    assert parsed.unnamed_omitted == 3
    assert {k: (t.callers_omitted, bool(t.unresolved)) for k, t in parsed.targets.items()} == {
        "c.py::h": (2, True),
        "d.py::k": (1, True),
    }
    assert parsed.truncated_unattributed == 0


def test_a_v2_use_inside_the_range_is_a_structured_reference_and_one_without_a_symbol_is_placed_by_its_lines():
    doc = v2(
        definitions=[
            definition(
                also_changed=[{"path": "t.py", "symbol": "test_f", "lines": "3-9"}, {"path": "lib/app.js", "lines": "5-9"}]
            ),
        ]
    )

    callers = parse_answer(doc, Resolve()).targets["a.py::f"].callers

    assert [(c.from_key, c.relation) for c in callers] == [
        ("t.py::test_f", "also_changed"),
        ("lib/app.js::span:5-9", "also_changed"),
    ]


def test_v2_roles_layout_only_changes_limits_and_unanalysed_files_are_read_from_their_places():
    doc = v2(
        unlisted={
            "entered_by": {"total": 1, "items": [{"def": {"path": "m.py", "symbol": "main", "lines": "1-4"}, "role": "main"}]},
            "layout_only": {"total": 1, "items": [{"path": "n.py", "symbol": "tidy", "lines": "2-3"}]},
            "no_caller": {"total": 1, "items": [{"path": "o.py", "symbol": "lonely", "lines": "1-2"}]},
        },
        coverage={
            "scope": {
                "complete": False,
                "gaps": [{"kind": "stopped", "reasons": ["deadline"]}, {"kind": "withheld", "files": 2}],
            },
            "limits": [{"what": "files"}],
            "unanalysed": {"total": 1, "items": [{"path": "big.py", "reason": "too large"}]},
        },
        empty=True,
    )

    parsed = parse_answer(doc, Resolve())

    assert {k: bool(t.unresolved) for k, t in parsed.targets.items()} == {"m.py::main": True, "n.py::tidy": True}
    assert len(parsed.limits) == 2 and "deadline" in parsed.limits[1]  # a stopped run is a limit; withheld files are not
    assert parsed.unanalysed == {"big.py"} and parsed.empty


# --- the two readers agree ----------------------------------------------------------------------------------------------


def def_ref(text):
    """The three spellings v1 wrote a definition in, as the engine's own `DefRef::parse` reads them."""
    if "::" in text:
        path, rest = text.split("::", 1)
        symbol, _, lines = rest.rpartition(" (") if " (" in rest else (rest, "", "")
        return {"path": path, "symbol": re.sub(r"\[\d+\]$", "", symbol), "lines": lines.rstrip(")")}
    path, _, lines = text.rpartition(":")
    return {"path": path, "lines": lines}


def to_v2(a):
    """What the engine's `ImpactV2::from_v1` makes of a v1 answer, field by field (MIGRATION.md)."""
    omitted_entry = next((e for e in a.get("changed", []) if e.get("kind") == "omitted"), None)
    definitions = []
    for e in a.get("changed", []):
        if e is omitted_entry:
            continue
        gaps = [{"kind": "not_traced"}] if e.get("unresolved") else []
        unread = [{"kind": "unread", "files": len(e["missed_in"])}] if e.get("missed_in") else []
        definitions.append(
            {
                "path": e["path"],
                "symbol": e.get("symbol"),
                "lines": e.get("lines", ""),
                "change": {"kind": "edited"},
                "entry": e.get("entry"),
                "rows": [
                    {
                        "path": c["path"],
                        "symbol": c.get("symbol"),
                        "lines": c.get("lines", ""),
                        "class": c.get("relation") if c.get("relation") in ("reference", "reexports") else "call",
                        "standing": "resolved" if c.get("evidence", "resolved") == "reexport" else c.get("evidence", "resolved"),
                        "guard": {"state": "none"},
                        "folded_by": c.get("folded_by"),
                        "kind": c.get("kind"),
                    }
                    for c in e.get("callers", [])
                ],
                "also_changed": [def_ref(t) for t in e.get("also_changed", [])],
                "coverage": {"calls": {"complete": not unread, "gaps": unread}, "reads": {"complete": True}},
                "gaps": gaps,
                "shed": {"resolved": int(e.get("callers_omitted", 0) or 0)},
            }
        )
    doc = v2(definitions=definitions, empty=a.get("empty", False), summary={"shed": a.get("truncated", 0)})
    if omitted_entry is not None:
        total = int(re.match(r"(\d+) changed", omitted_entry["unresolved"])[1])
        items = [
            {
                "def": def_ref(o["definition"] + f" ({o.get('lines', '')})"),
                "shed": {"resolved": int(o.get("callers_omitted", 0) or 0)},
            }
            for o in omitted_entry.get("omitted", [])
        ]
        doc["omitted"] = {"definitions": {"total": total, "items": items}, "needs": 0}
    roles = []
    for text in a.get("entry_unlisted", []):
        roles.append({"def": def_ref(text), "role": "runner"})
    doc["unlisted"] = {
        "entered_by": {"total": len(roles), "items": roles},
        "layout_only": {"total": len(a.get("layout_only", [])), "items": [def_ref(t) for t in a.get("layout_only", [])]},
    }
    doc["coverage"] = {
        "scope": {"complete": True},
        "limits": a.get("limits", []),
        "unanalysed": {"total": len(a.get("unanalysed_files", [])), "items": a.get("unanalysed_files", [])},
    }
    return doc


def what_the_scorer_sees(parsed):
    """v1 wrote `reexport` as evidence where v2 says resolved with class reexports (the scorer ranks them alike), and its
    omitted entry became a target of the pseudo-path `(changed definitions)`, which v2 has no entry for."""
    return {
        "targets": {
            key: {
                "callers": [
                    (c.from_key, "resolved" if c.evidence == "reexport" else c.evidence, *dataclasses.astuple(c)[2:])
                    for c in t.callers
                ],
                "partial": bool(t.unresolved or t.missed_in or t.callers_omitted),
                "callers_omitted": t.callers_omitted,
                "entry": t.entry,
            }
            for key, t in parsed.targets.items()
            if not key.startswith("(changed definitions)")
        },
        "limits": parsed.limits,
        "unanalysed": parsed.unanalysed,
        "empty": parsed.empty,
        "unnamed_omitted": parsed.unnamed_omitted,
        "truncated": parsed.truncated,
        "truncated_unattributed": parsed.truncated_unattributed,
    }


@pytest.mark.parametrize("case", FIXTURES, ids=[f"{c['engine']}-{i}" for i, c in enumerate(FIXTURES)])
def test_the_v2_form_of_a_v1_answer_is_scored_as_the_v1_form_is(case):
    older = what_the_scorer_sees(parse_answer(case["answer"], Resolve()))
    newer = what_the_scorer_sees(parse_answer(to_v2(case["answer"]), Resolve()))

    assert newer == older
