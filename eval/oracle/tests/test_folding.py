from pathlib import Path

import pytest

from delta import Delta
from folding import fold_rule, mark_folding, runtime_annotation
from model import Relation
from scipindex import Index
from truth import Snapshot

EMPTY = Snapshot(Path("."), Index(), "python")

KINDS = ("calls", "instantiates", "passes_as_value", "reads", "writes", "overrides", "imports", "reexports", "type_use")


@pytest.mark.parametrize("change", ["added", "removed", "renamed", "moved", "visibility", "unknown", "declaration", "other"])
def test_nothing_is_folded_after_a_change_the_table_never_folds(change):
    for shape in ("callable", "class", "type", "variable"):
        assert all(fold_rule(Delta(change, shape, typed=True), kind) is None for kind in KINDS)


def test_a_doc_or_layout_edit_of_an_undecorated_callable_folds_every_relation():
    undecorated = Delta("doc_layout", "callable")

    assert {fold_rule(undecorated, kind) for kind in KINDS} == {"no-code-change"}


def test_a_decorator_may_read_the_docstring_so_a_decorated_callable_or_a_class_folds_nothing():
    assert all(fold_rule(Delta("doc_layout", "callable", decorated=True), kind) is None for kind in KINDS)
    assert all(fold_rule(Delta("doc_layout", "class"), kind) is None for kind in KINDS)


def test_marking_keeps_the_rule_on_each_relation_and_affected_means_no_rule():
    relations = [Relation("a::f", "a::g", "calls", 3), Relation("b::h", "a::g", "imports", 1)]

    marked = mark_folding(relations, Delta("doc_layout", "callable"), EMPTY)

    assert [r.rule for r in marked] == ["no-code-change", "no-code-change"]
    assert [r.rule for r in mark_folding(relations, Delta("body", "callable"), EMPTY)] == [None, "imports-a-body"]


def folded(delta: Delta) -> dict[str, str]:
    return {kind: rule for kind in KINDS if (rule := fold_rule(delta, kind))}


def test_a_body_edit_folds_what_a_body_cannot_reach_and_never_the_uses_of_the_callable():
    assert folded(Delta("body", "callable")) == {
        "overrides": "override-not-run",
        "imports": "imports-a-body",
        "reexports": "imports-a-body",
    }


def test_a_type_use_folds_after_a_body_edit_only_when_every_type_is_declared():
    assert folded(Delta("body", "callable", typed=True))["type_use"] == "declared-type"
    assert "type_use" not in folded(Delta("body", "callable", typed=False))
    assert fold_rule(Delta("body", "callable", typed=True, constructor=True), "type_use") == "declared-type"
    assert fold_rule(Delta("body", "callable", typed=True), "type_use", runtime_annotation=True) is None


def test_a_compatible_signature_change_folds_calls_to_check_the_default_and_imports():
    assert folded(Delta("signature_compatible", "callable", typed=True, parameters=("c = 1",))) == {
        "calls": "check-the-default",
        "instantiates": "check-the-default",
        "imports": "imports-a-body",
        "reexports": "imports-a-body",
    }


def test_any_other_signature_change_folds_only_imports():
    assert folded(Delta("signature", "callable", typed=True)) == {"imports": "imports-a-body", "reexports": "imports-a-body"}


def test_changed_decorators_and_a_class_body_fold_only_imports_whatever_the_definition_is():
    imports = {"imports": "imports-a-body", "reexports": "imports-a-body"}

    assert folded(Delta("decorators", "callable", decorated=True)) == imports
    assert folded(Delta("decorators", "class", decorated=True)) == imports
    assert folded(Delta("class_body", "class")) == imports


def test_a_variable_value_folds_imports_and_a_declared_type_use_but_never_its_reads():
    assert folded(Delta("value", "variable", typed=True)) == {
        "imports": "imports-a-body",
        "reexports": "imports-a-body",
        "type_use": "declared-type",
    }
    assert "type_use" not in folded(Delta("value", "variable", typed=False))
    assert not {"reads", "writes", "passes_as_value"} & set(folded(Delta("value", "variable", typed=True)))


def python_snapshot(tmp_path: Path, source: str) -> Snapshot:
    (tmp_path / "m.py").write_text(source)
    return Snapshot(tmp_path, Index(), "python")


def type_use(from_key: str, site: int) -> Relation:
    return Relation(f"m.py::{from_key}", "m.py::T", "type_use", site)


def test_a_python_annotation_a_class_or_a_decorator_executes_is_never_a_declared_type_fold(tmp_path: Path):
    plain = "class C:\n    field: T = None\n\n    def m(self, a: T) -> T:\n        return a\n\n\n@route\ndef h(a: T) -> int:\n    return 1\n"
    snapshot = python_snapshot(tmp_path, plain)

    assert runtime_annotation(snapshot, type_use("C", 2))  # a class body's field annotation
    assert not runtime_annotation(snapshot, type_use("C.m", 4))  # a method's own signature, undecorated
    assert runtime_annotation(snapshot, type_use("h", 9))  # the signature of a decorated function
    assert not runtime_annotation(snapshot, Relation("m.py::C.m", "m.py::T", "calls", 4))


def test_every_annotation_in_a_file_that_imports_a_run_time_annotation_library_is_executed(tmp_path: Path):
    for library in ("pydantic", "fastapi", "dataclasses", "attrs", "marshmallow", "typer", "click"):
        snapshot = python_snapshot(tmp_path, f"from {library} import x\n\n\ndef f(a: T) -> int:\n    return 1\n")
        assert runtime_annotation(snapshot, type_use("f", 4)), library
    plain = python_snapshot(tmp_path, "import json\n\n\ndef f(a: T) -> int:\n    return 1\n")
    assert not runtime_annotation(plain, type_use("f", 4))


def test_a_run_time_annotation_is_not_folded_after_a_body_edit_of_a_typed_callable(tmp_path: Path):
    snapshot = python_snapshot(tmp_path, "from pydantic import BaseModel\n\n\ndef f(a: T) -> int:\n    return 1\n")
    marked = mark_folding([type_use("f", 4)], Delta("body", "callable", typed=True), snapshot)

    assert marked[0].rule is None
    elsewhere = python_snapshot(tmp_path, "def f(a: T) -> int:\n    return 1\n")
    assert mark_folding([type_use("f", 1)], Delta("body", "callable", typed=True), elsewhere)[0].rule == "declared-type"
