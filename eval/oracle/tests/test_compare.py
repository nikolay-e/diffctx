from compare import compare, make_resolver, parse_answer
from model import MODULE, Definition, Relation

TARGET = Definition("shop/pricing.py", "function", "total", (), 1, 3, 0, 4, "()", "h", "s")


def caller_def(path, name, start, end):
    return Definition(path, "function", name, (), start, end, start - 1, 4, "()", name, name)


DEFS = {
    "shop/checkout.py": [caller_def("shop/checkout.py", "charge", 1, 3), caller_def("shop/checkout.py", "refund", 5, 8)],
    "shop/report.py": [caller_def("shop/report.py", "build", 1, 4)],
    "shop/pricing.py": [TARGET],
}
RESOLVE = make_resolver(lambda path: DEFS.get(path, []))


def answer(callers, **extra):
    entry = {"path": "shop/pricing.py", "symbol": "total", "lines": "1-3", "kind": "function", "callers": callers, **extra}
    return parse_answer({"changed": [entry]}, RESOLVE)


def caller(path, symbol, lines, **extra):
    return {"path": path, "symbol": symbol, "lines": lines, "weight": 1.0, "evidence": "resolved", **extra}


def relation(from_key, kind="calls", in_diff=False):
    return Relation(from_key, TARGET.key, kind, 2, in_diff=in_diff)


def test_exact_match_is_true_positive_and_extra_is_false_positive():
    found = answer([caller("shop/checkout.py", "charge", "1-3"), caller("shop/report.py", "build", "1-4")])

    cells, details = compare(TARGET, [relation("shop/checkout.py::charge")], found)

    assert len(cells) == 1
    calls = cells[0]
    assert (calls["truth"], calls["tp"], calls["fn"], calls["fp"], calls["fcn"]) == (1, 1, 0, 1, False)
    assert {d["verdict"] for d in details} == {"tp", "fp"}


def test_missing_caller_is_false_negative_and_empty_unexplained_answer_is_false_complete_negative():
    cells, _ = compare(TARGET, [relation("shop/checkout.py::charge")], answer([]))

    assert (cells[0]["fn"], cells[0]["fcn"], cells[0]["abstained"]) == (1, True, False)


def test_a_named_unknown_is_abstention_not_a_false_complete_negative():
    found = answer([], unresolved="`total` is ambiguous: 3 python definitions")

    cells, _ = compare(TARGET, [relation("shop/checkout.py::charge")], found)

    assert (cells[0]["fn"], cells[0]["fcn"], cells[0]["abstained"]) == (1, False, True)


def test_a_limit_or_an_unanalysed_file_is_abstention():
    limited = parse_answer({"changed": [], "limits": ["deadline"]}, RESOLVE)
    unanalysed = parse_answer({"changed": [], "unanalysed_files": [{"path": "shop/pricing.py"}]}, RESOLVE)

    for found in (limited, unanalysed):
        cells, _ = compare(TARGET, [relation("shop/checkout.py::charge")], found)
        assert (cells[0]["fcn"], cells[0]["abstained"]) == (False, True)


def test_candidates_are_weak_not_claims():
    found = answer(
        [
            caller("shop/checkout.py", "charge", "1-3", evidence="candidate"),
            caller("shop/report.py", "build", "1-4", evidence="name"),
        ]
    )

    cells, _ = compare(TARGET, [relation("shop/checkout.py::charge")], found)

    total = {k: sum(c[k] for c in cells) for k in ("tp", "tp_weak", "fp", "fp_weak", "fn")}
    assert total == {"tp": 0, "tp_weak": 1, "fp": 0, "fp_weak": 1, "fn": 0}


def test_relations_inside_the_diff_are_not_scored_either_way():
    found = answer([caller("shop/checkout.py", "charge", "1-3")])

    cells, details = compare(TARGET, [relation("shop/checkout.py::charge", in_diff=True)], found)

    assert cells == [] and details == []


def test_kind_mismatch_is_counted_not_failed():
    found = answer([caller("shop/checkout.py", "charge", "1-3", relation="reference")])

    cells, _ = compare(TARGET, [relation("shop/checkout.py::charge", "calls")], found)

    assert (cells[0]["tp"], cells[0]["kind_mismatches"]) == (1, 1)


def test_a_reexport_row_is_a_sure_answer_for_a_reexports_relation():
    row = {"path": "pkg/index.ts", "lines": "1-1", "weight": 1.0, "evidence": "reexport", "relation": "reexports"}
    found = answer([row])
    truth = [relation("pkg/index.ts::" + MODULE, "reexports")]

    cells, _ = compare(TARGET, truth, found)

    assert (cells[0]["kind"], cells[0]["tp"], cells[0]["tp_weak"], cells[0]["kind_mismatches"]) == ("reexports", 1, 0, 0)


def test_one_pair_with_several_kinds_is_scored_once_under_the_strongest():
    truth = [relation("shop/checkout.py::charge", "reads"), relation("shop/checkout.py::charge", "calls")]

    cells, _ = compare(TARGET, truth, answer([caller("shop/checkout.py", "charge", "1-3")]))

    assert [(c["kind"], c["truth"], c["tp"]) for c in cells] == [("calls", 1, 1)]


def test_resolver_picks_by_name_and_overlap_and_falls_back():
    twins = {"m.py": [caller_def("m.py", "run", 1, 4), caller_def("m.py", "run", 10, 14)]}
    resolve = make_resolver(lambda path: twins.get(path, []))

    assert resolve("m.py", "run", "11-13") == "m.py::run"
    assert resolve("m.py", None, None) == "m.py::<module>"
    assert resolve("n.py", "ghost", "1-2") == "n.py::ghost"


def test_a_symbol_the_cap_cut_or_shed_is_partial_and_an_uncapped_one_with_a_miss_is_not():
    truth = [relation("shop/checkout.py::charge"), relation("shop/report.py::build")]
    listed = [caller("shop/checkout.py", "charge", "1-3")]

    cut = answer(listed, callers_omitted=1)
    cells, _ = compare(TARGET, truth, cut)
    assert (cells[0]["tp"], cells[0]["fn"], cells[0]["fcn"], cells[0]["abstained"]) == (1, 1, False, True)

    cut_to_nothing = answer([], callers_omitted=2)
    cells, _ = compare(TARGET, truth, cut_to_nothing)
    assert (cells[0]["fn"], cells[0]["fcn"], cells[0]["abstained"]) == (2, False, True)

    stub = answer([], unresolved="omitted to fit the cap: callers not listed")
    cells, _ = compare(TARGET, truth, stub)
    assert (cells[0]["fn"], cells[0]["fcn"], cells[0]["abstained"]) == (2, False, True)

    uncapped = answer(listed)
    cells, _ = compare(TARGET, truth, uncapped)
    assert (cells[0]["tp"], cells[0]["fn"], cells[0]["abstained"]) == (1, 1, False)

    silent = answer([])
    cells, _ = compare(TARGET, truth, silent)
    assert (cells[0]["fn"], cells[0]["fcn"], cells[0]["abstained"]) == (2, True, False)


def test_definitions_shed_into_the_one_omitted_entry_are_partial_answers():
    data = {
        "changed": [
            {
                "path": "(changed definitions)",
                "kind": "omitted",
                "unresolved": "1 changed definition(s) omitted: the answer cap; --budget 300 shows them",
                "callers_omitted": 2,
                "omitted": [{"definition": "shop/checkout.py::charge", "callers_omitted": 2}],
            }
        ]
    }
    parsed = parse_answer(data, make_resolver(lambda _path: []))
    shed = parsed.targets["shop/checkout.py::charge"]
    assert (shed.callers_omitted, bool(shed.unresolved)) == (2, True)


def test_a_shed_definition_is_found_by_its_lines_and_unnamed_ones_make_a_missing_target_partial():
    twins = [caller_def("shop/pricing.py", "send", 1, 3), caller_def("shop/pricing.py", "send", 10, 14)]
    resolve = make_resolver(lambda path: twins if path == "shop/pricing.py" else [])
    data = {
        "changed": [
            {
                "path": "(changed definitions)",
                "kind": "omitted",
                "unresolved": "3 changed definition(s) omitted: the answer cap; --budget 300 shows them",
                "callers_omitted": 4,
                "omitted": [{"definition": "shop/pricing.py::send", "lines": "10-14", "callers_omitted": 4}],
            }
        ]
    }
    parsed = parse_answer(data, resolve)
    assert "shop/pricing.py::send" in parsed.targets
    assert parsed.unnamed_omitted == 2
    missing = Definition("shop/other.py", "function", "other", (), 1, 3, 0, 4, "()", "h", "s")
    cells, _ = compare(missing, [Relation("shop/checkout.py::charge", missing.key, "calls", 2)], parsed)
    assert (cells[0]["fcn"], cells[0]["abstained"]) == (False, True)


def test_a_role_or_layout_only_listing_is_no_absence_claim_and_a_constructor_is_judged_by_its_class():
    data = {
        "changed": [{"path": "shop/pricing.py", "symbol": "Cart", "lines": "1-3", "kind": "class", "callers": []}],
        "entry_unlisted": ['shop/checkout.py::charge (1-3) — run as a script (if __name__ == "__main__")'],
        "layout_only": ["shop/checkout.py::refund (5-8)"],
    }
    parsed = parse_answer(data, RESOLVE)
    for name in ("charge", "refund"):
        assert parsed.targets[f"shop/checkout.py::{name}"].unresolved
    init = Definition("shop/pricing.py", "function", "__init__", ("Cart",), 2, 3, 1, 4, "()", "h", "s")
    cells, _ = compare(init, [Relation("shop/report.py::build", init.key, "instantiates", 2)], parsed)
    assert (cells[0]["fn"], cells[0]["fcn"]) == (1, True)


def test_a_truncation_no_entry_owns_makes_every_target_a_partial_answer():
    # 1.20.0 reports the callers its cap dropped as one top-level count; no entry says which symbol lost them.
    truth = [relation("shop/checkout.py::charge"), relation("shop/report.py::build")]
    entry = {"path": "shop/pricing.py", "symbol": "total", "lines": "1-3", "kind": "function", "callers": []}
    capped = parse_answer({"changed": [entry], "truncated": 5}, RESOLVE)

    cells, _ = compare(TARGET, truth, capped)

    assert (cells[0]["fn"], cells[0]["fcn"], cells[0]["abstained"]) == (2, False, True)
    # a target the answer does not list at all is partial too
    elsewhere = Definition("shop/other.py", "function", "other", (), 1, 3, 0, 4, "()", "o", "o")
    cells, _ = compare(elsewhere, [Relation("shop/report.py::build", elsewhere.key, "calls", 2)], capped)
    assert (cells[0]["fcn"], cells[0]["abstained"]) == (False, True)


def test_a_truncation_a_symbol_owns_marks_only_that_symbol_and_no_truncation_marks_none():
    truth = [relation("shop/checkout.py::charge")]
    entry = {"path": "shop/pricing.py", "symbol": "total", "lines": "1-3", "kind": "function", "callers": []}
    other = Definition("shop/other.py", "function", "other", (), 1, 3, 0, 4, "()", "o", "o")
    other_truth = [Relation("shop/report.py::build", other.key, "calls", 2)]
    other_entry = {
        "path": "shop/other.py",
        "symbol": "other",
        "lines": "1-3",
        "kind": "function",
        "callers": [caller("shop/report.py", "build", "1-4")],
    }
    resolve = make_resolver(lambda path: {**DEFS, "shop/other.py": [other]}.get(path, []))

    owned = parse_answer({"changed": [{**entry, "callers_omitted": 1}, other_entry], "truncated": 1}, resolve)
    whole = parse_answer({"changed": [entry]}, RESOLVE)

    assert compare(TARGET, truth, owned)[0][0]["abstained"] is True
    assert compare(other, other_truth, owned)[0][0]["abstained"] is False
    assert compare(TARGET, truth, whole)[0][0]["fcn"] is True


def test_a_type_use_and_an_import_are_scored_under_their_own_kind_not_as_reads():
    truth = [relation("shop/checkout.py::charge", "type_use"), relation("shop/report.py::build", "imports")]

    cells, details = compare(TARGET, truth, answer([caller("shop/checkout.py", "charge", "1-3")]))

    assert {(c["kind"], c["truth"], c["tp"], c["fn"]) for c in cells} == {("type_use", 1, 1, 0), ("imports", 1, 0, 1)}
    assert {d["kind"] for d in details} == {"type_use", "imports"}


def test_an_engine_call_row_on_a_class_target_is_scored_as_an_instantiation():
    shop = Definition("shop/pricing.py", "class", "Shop", (), 1, 3, 0, 6, "()", "h", "s")
    wrong = answer([caller("shop/report.py", "build", "1-4", relation="call")])
    wrong.targets[shop.key] = wrong.targets.pop(TARGET.key)

    cells, details = compare(shop, [], wrong)

    assert [(c["kind"], c["fp"]) for c in cells] == [("instantiates", 1)]
    assert details[0]["moved"] == ["R10"]
    plain = answer([caller("shop/report.py", "build", "1-4", relation="call")])
    assert [(c["kind"], c["fp"]) for c in compare(TARGET, [], plain)[0]] == [("calls", 1)]


def test_a_constructor_is_scored_under_its_class_whichever_the_language_names_it():
    errors = "source/errors.ts"
    cls = Definition(errors, "class", "RequestError", (), 1, 20, 0, 6, "()", "c", "c")
    ctor = Definition(errors, "method", "constructor", ("RequestError",), 3, 8, 2, 2, "()", "k", "k")
    caller_def = Definition("source/index.ts", "function", "fail", (), 1, 4, 0, 9, "()", "f", "f")
    resolve = make_resolver(lambda path: {errors: [cls, ctor], "source/index.ts": [caller_def]}.get(path, []))
    entry = {"path": errors, "symbol": "RequestError", "lines": "1-20", "kind": "class", "callers": []}
    entry["callers"] = [{"path": "source/index.ts", "symbol": "fail", "lines": "1-4", "weight": 1.0, "evidence": "resolved"}]
    reported = parse_answer({"changed": [entry]}, resolve)
    truth = [Relation(caller_def.key, ctor.key, "instantiates", 2)]

    cells, _ = compare(ctor, truth, reported)

    assert [(c["kind"], c["truth"], c["tp"], c["fn"], c["fcn"]) for c in cells] == [("instantiates", 1, 1, 0, False)]


def test_a_use_the_engine_lists_as_also_changed_counts_as_the_relation():
    truth = [relation("shop/checkout.py::charge")]
    entry = {
        "path": "shop/pricing.py",
        "symbol": "total",
        "lines": "1-3",
        "kind": "function",
        "callers": [],
        "also_changed": ["shop/checkout.py::charge (1-3)", "shop/report.py::build[7] (1-4)"],
    }
    listed = parse_answer({"changed": [entry]}, RESOLVE)

    cells, details = compare(TARGET, truth, listed)

    assert [(c["kind"], c["tp"], c["fp"], c["fcn"], c["kind_mismatches"]) for c in cells] == [
        ("calls", 1, 0, False, 0),
        ("reads", 0, 1, False, 0),
    ]
    assert {d["verdict"]: d["moved"] for d in details} == {"tp": [], "fp": ["R14"]}
    silent = answer([])
    assert compare(TARGET, truth, silent)[0][0]["fn"] == 1


def test_an_entry_or_a_truncation_disclosed_with_nothing_listed_is_an_abstention_not_a_false_complete_negative():
    truth = [relation("shop/checkout.py::charge")]
    entry = {"path": "shop/pricing.py", "symbol": "total", "lines": "1-3", "kind": "function", "callers": []}
    entered = parse_answer({"changed": [{**entry, "entry": 'run as a script (if __name__ == "__main__")'}]}, RESOLVE)
    cut = parse_answer({"changed": [entry], "truncated": 4}, RESOLVE)
    cut_elsewhere = parse_answer({"changed": [{**entry, "callers_omitted": 1}], "truncated": 1}, RESOLVE)

    for disclosed in (entered, cut, cut_elsewhere):
        cells, _ = compare(TARGET, truth, disclosed)
        assert (cells[0]["fn"], cells[0]["fcn"], cells[0]["abstained"]) == (1, False, True)


def test_a_row_that_states_its_kind_is_scored_under_it_and_a_wrong_kind_is_counted():
    truth = [relation("shop/checkout.py::charge", "type_use")]
    stated = answer(
        [
            caller("shop/checkout.py", "charge", "1-3", relation="reference", kind="calls"),
            caller("shop/report.py", "build", "1-4", relation="reference", kind="type_use"),
        ]
    )

    cells, details = compare(TARGET, truth, stated)

    assert {(c["kind"], c["tp"], c["fp"], c["kind_mismatches"]) for c in cells} == {("type_use", 1, 1, 1)}
    assert {d["kind"] for d in details} == {"type_use"}
    unstated = answer([caller("shop/report.py", "build", "1-4", relation="reference")])
    assert [c["kind"] for c in compare(TARGET, [], unstated)[0]] == ["reads"]


def test_a_truth_pair_the_folding_table_folds_is_not_an_affected_one_and_recall_is_taken_over_the_affected():
    folded = Relation("shop/checkout.py::charge", TARGET.key, "calls", 2, rule="no-code-change")
    affected = Relation("shop/report.py::build", TARGET.key, "calls", 3)

    cells, details = compare(TARGET, [folded, affected], answer([caller("shop/report.py", "build", "1-4")]))

    [cell] = cells
    assert (cell["truth"], cell["tp"], cell["truth_affected"], cell["tp_affected"]) == (2, 1, 1, 1)
    assert {d["from"]: (d["affected"], d["rule"]) for d in details} == {
        "shop/checkout.py::charge": (False, "no-code-change"),
        "shop/report.py::build": (True, None),
    }
    listed = answer([caller("shop/checkout.py", "charge", "1-3")])
    [only] = compare(TARGET, [folded, affected], listed)[0]
    assert (only["tp"], only["tp_affected"], only["truth_affected"]) == (1, 0, 1)


def test_a_truth_pair_the_engine_folded_and_the_oracle_says_is_affected_is_a_false_fold():
    affected = Relation("shop/checkout.py::charge", TARGET.key, "calls", 2)
    unaffected = Relation("shop/report.py::build", TARGET.key, "calls", 3, rule="no-code-change")
    rows = [
        caller("shop/checkout.py", "charge", "1-3", folded_by="no-code-change"),
        caller("shop/report.py", "build", "1-4", folded_by="no-code-change"),
    ]

    cells, details = compare(TARGET, [affected, unaffected], answer(rows))

    [cell] = cells
    assert (cell["tp"], cell["folded"], cell["false_fold"], cell["tp_affected"]) == (2, 2, 1, 0)
    assert {d["from"]: (d["folded_by"], d["false_fold"]) for d in details} == {
        "shop/checkout.py::charge": ("no-code-change", True),
        "shop/report.py::build": ("no-code-change", False),
    }
    listed = answer([caller("shop/checkout.py", "charge", "1-3")])
    [plain] = compare(TARGET, [affected], listed)[0]
    assert (plain["folded"], plain["false_fold"], plain["tp_affected"]) == (0, 0, 1)


def test_silence_about_relations_the_change_cannot_reach_is_no_false_complete_negative():
    imported = Relation("shop/__init__.py::<module>", TARGET.key, "imports", 12, rule="imports-a-body")
    called = Relation("shop/checkout.py::charge", TARGET.key, "calls", 2)

    [only_import] = compare(TARGET, [imported], answer([]))[0]
    cells = {c["kind"]: c for c in compare(TARGET, [imported, called], answer([]))[0]}

    assert (only_import["truth"], only_import["truth_affected"], only_import["fcn"]) == (1, 0, False)
    assert (cells["imports"]["fcn"], cells["calls"]["fcn"]) == (False, True)


def test_an_also_changed_entry_spelled_path_and_lines_names_the_definition_that_holds_them():
    truth = [relation("shop/checkout.py::charge"), relation("shop/report.py::build")]
    entry = {
        "path": "shop/pricing.py",
        "symbol": "total",
        "lines": "1-3",
        "kind": "function",
        "callers": [],
        "also_changed": ["shop/checkout.py:1-3", "shop/report.py:2", "shop/elsewhere.py:5-9"],
    }
    listed = parse_answer({"changed": [entry]}, RESOLVE)

    cells, details = compare(TARGET, truth, listed)

    assert {(d["from"], d["verdict"]) for d in details} == {
        ("shop/checkout.py::charge", "tp"),
        ("shop/report.py::build", "tp"),
        ("shop/elsewhere.py::<module>", "fp"),
    }
    assert [c["fcn"] for c in cells] == [False, False]


def test_a_candidate_folded_as_an_untyped_receiver_claims_nothing_so_it_is_no_false_fold():
    affected = Relation("shop/checkout.py::charge", TARGET.key, "calls", 2)
    rows = [caller("shop/checkout.py", "charge", "1-3", evidence="candidate", folded_by="untyped-receiver")]

    [cell], [detail] = compare(TARGET, [affected], answer(rows))

    assert (cell["folded"], cell["false_fold"], cell["tp_weak"]) == (1, 0, 1)
    assert detail["false_fold"] is False


def test_a_pair_is_folded_only_when_every_row_the_answer_has_for_it_is():
    reads = Relation("shop/checkout.py::charge", TARGET.key, "reads", 9)
    exported = Relation("shop/checkout.py::charge", TARGET.key, "reexports", 2)
    folded_first = [
        caller("shop/checkout.py", "charge", "1-3", relation="reexports", folded_by="imports-a-body"),
        caller("shop/checkout.py", "charge", "1-3", relation="reference"),
    ]

    [cell], [detail] = compare(TARGET, [reads, exported], answer(folded_first))
    only_folded = answer([folded_first[0], {**folded_first[0], "relation": "reexports"}])
    [whole], [row] = compare(TARGET, [exported], only_folded)

    assert (cell["folded"], cell["false_fold"], detail["folded_by"]) == (0, 0, None)  # a listed row answers for the pair
    assert (whole["folded"], row["folded_by"]) == (1, "imports-a-body")  # every row folded: the pair is


def test_an_answer_with_no_entry_for_the_target_claims_no_absence():
    found_nothing = parse_answer(
        {"schema": "diffctx.impact.v1", "changed": [], "symbol": "total", "symbol_named": {"files": 1, "rows": []}}, RESOLVE
    )
    other_entry = parse_answer(
        {"changed": [{"path": "shop/checkout.py", "symbol": "charge", "lines": "1-3", "kind": "function", "callers": []}]},
        RESOLVE,
    )

    for found in (found_nothing, other_entry):
        cells, _ = compare(TARGET, [relation("shop/checkout.py::charge")], found)
        assert (cells[0]["fn"], cells[0]["fcn"], cells[0]["abstained"]) == (1, False, False)


def test_same_named_overloads_are_scored_apart_by_their_spans():
    first = Definition("shop/pricing.py", "method", "total", ("Cart",), 1, 3, 0, 4, "(int)", "h1", "s1", overloaded=True)
    second = Definition("shop/pricing.py", "method", "total", ("Cart",), 5, 8, 4, 4, "(str)", "h2", "s2", overloaded=True)
    defs = {
        "shop/checkout.py": [caller_def("shop/checkout.py", "charge", 1, 3), caller_def("shop/checkout.py", "refund", 5, 8)],
        "shop/pricing.py": [first, second],
    }
    resolve = make_resolver(lambda path: defs.get(path, []))
    charges = [caller("shop/checkout.py", "charge", "1-3")]
    refunds = [caller("shop/checkout.py", "refund", "5-8")]
    entries = [
        {"path": "shop/pricing.py", "symbol": "Cart.total", "lines": "1-3", "kind": "method", "callers": charges},
        {"path": "shop/pricing.py", "symbol": "Cart.total", "lines": "5-8", "kind": "method", "callers": refunds},
    ]
    found = parse_answer({"changed": entries}, resolve)

    assert first.key != second.key
    for target, user, site in ((first, "charge", 2), (second, "refund", 6)):
        cells, _ = compare(target, [Relation(f"shop/checkout.py::{user}", target.key, "calls", site)], found)
        assert (cells[0]["truth"], cells[0]["tp"], cells[0]["fp"], cells[0]["fn"]) == (1, 1, 0, 0)
