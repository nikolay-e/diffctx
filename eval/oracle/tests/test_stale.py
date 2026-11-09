from pathlib import Path

from changed import ChangedDefinition
from model import Relation
from scipindex import Index
from stale import pairing, renamed_answer, still_named
from truth import Snapshot

BASE = """def _old():
    return 1


def caller():
    return _old()


def rewritten():
    return _old()


def untouched():
    return _old()


def gone():
    return _old()
"""
HEAD = """def caller():
    return 2


def rewritten():
    return _old()


def untouched():
    return _old()
"""


def use(name: str) -> Relation:
    return Relation(f"m.py::{name}", "m.py::_old", "calls", 2)


def test_a_removed_definition_is_scored_against_head_where_only_what_still_names_it_is_stale(tmp_path: Path):
    snapshots = {}
    for side, text in (("base", BASE), ("head", HEAD)):
        (tmp_path / side).mkdir()
        (tmp_path / side / "m.py").write_text(text)
        snapshots[side] = Snapshot(tmp_path / side, Index(), "python")
    base, head = snapshots["base"], snapshots["head"]
    by_name = lambda snap, n: next(d for d in snap.definitions("m.py") if d.name == n)  # noqa: E731
    changed = [
        ChangedDefinition("removed", None, by_name(base, "_old")),
        ChangedDefinition("modified", by_name(head, "caller"), by_name(base, "caller")),
        ChangedDefinition("modified", by_name(head, "rewritten"), by_name(base, "rewritten")),
        ChangedDefinition("removed", None, by_name(base, "gone")),
    ]
    users = [use(n) for n in ("caller", "rewritten", "untouched", "gone")] + [
        Relation("m.py::<module>", "m.py::_old", "reads", 1)
    ]

    kept = still_named(users, head, changed, {"_old"})

    assert [r.from_key for r in kept] == ["m.py::rewritten", "m.py::untouched"]


RENAMED_BASE = "class Base: pass\n\n\nclass ResponseClosed(Base):\n    def __init__(self):\n        self.message = 'closed'\n\n\ndef read():\n    raise ResponseClosed()\n"
RENAMED_HEAD = RENAMED_BASE.replace("ResponseClosed", "StreamClosed")


def test_a_removal_the_answer_pairs_with_a_head_definition_of_the_same_body_is_scored_under_the_pair(tmp_path: Path):
    from compare import compare, make_resolver, parse_answer

    snapshots = {}
    for side, text in (("base", RENAMED_BASE), ("head", RENAMED_HEAD)):
        (tmp_path / side).mkdir()
        (tmp_path / side / "m.py").write_text(text)
        snapshots[side] = Snapshot(tmp_path / side, Index(), "python")
    base, head = snapshots["base"], snapshots["head"]
    removed = next(d for d in base.definitions("m.py") if d.name == "ResponseClosed" and d.kind == "class")
    constructor = next(d for d in base.definitions("m.py") if d.qualname == "ResponseClosed.__init__")
    entry = {"path": "m.py", "symbol": "StreamClosed", "lines": "4-6", "kind": "class", "callers": []}
    entry["callers"] = [{"path": "m.py", "symbol": "read", "lines": "9-10", "weight": 1.0, "evidence": "resolved"}]
    answer = parse_answer({"changed": [entry]}, make_resolver(head.definitions, base.definitions))
    truth = [Relation("m.py::read", removed.key, "instantiates", 10)]

    assert pairing(head, base, removed, list(answer.targets)) == ("m.py::StreamClosed", "m.py::ResponseClosed")
    assert pairing(head, base, constructor, list(answer.targets)) == ("m.py::StreamClosed", "m.py::ResponseClosed")
    unpaired, _ = compare(removed, truth, answer)
    paired = renamed_answer(answer, "m.py::ResponseClosed", "m.py::StreamClosed")
    cells, _ = compare(removed, truth, paired)

    assert (unpaired[0]["fn"], unpaired[0]["fcn"]) == (1, True)
    assert (cells[0]["tp"], cells[0]["fcn"]) == (1, False)
    kept = still_named(truth, head, [ChangedDefinition("removed", None, removed)], {"ResponseClosed", "StreamClosed"})
    assert [r.from_key for r in kept] == ["m.py::read"]  # the user's head version names the new name


def test_a_removal_whose_body_differs_once_the_name_is_set_aside_is_not_paired(tmp_path: Path):
    (tmp_path / "base").mkdir()
    (tmp_path / "head").mkdir()
    (tmp_path / "base" / "m.py").write_text(RENAMED_BASE)
    (tmp_path / "head" / "m.py").write_text(RENAMED_HEAD.replace("'closed'", "'shut'"))
    base, head = Snapshot(tmp_path / "base", Index(), "python"), Snapshot(tmp_path / "head", Index(), "python")
    removed = next(d for d in base.definitions("m.py") if d.name == "ResponseClosed" and d.kind == "class")

    assert pairing(head, base, removed, ["m.py::StreamClosed"]) is None
