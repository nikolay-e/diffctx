from compare import compare, make_resolver, parse_answer
from model import Definition, Relation

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
    assert {d["status"] for d in details} == {"tp", "fp"}


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
