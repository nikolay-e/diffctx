"""The folding table of Revision 12.1, applied to the oracle's own classification of a change (`delta.py`).

A truth relation is *affected* unless the table says the change cannot reach it; the rule that would fold it is kept
on the row. Where the table is silent a relation is affected: when unsure, nothing folds.
"""

from __future__ import annotations

import re
from dataclasses import replace

from delta import Delta
from model import Relation
from truth import Snapshot

RUNTIME_LIBRARIES = ("pydantic", "fastapi", "dataclasses", "attrs", "attr", "marshmallow", "typer", "click")
RUNTIME_IMPORT = re.compile(rf"(?m)^\s*(?:from|import)\s+(?:{'|'.join(RUNTIME_LIBRARIES)})\b")
IMPORT_KINDS = ("imports", "reexports")
NEVER_FOLDED = {"added", "removed", "renamed", "moved", "visibility", "unknown", "declaration", "other"}


def fold_rule(delta: Delta, kind: str, runtime_annotation: bool = False) -> str | None:
    """The rule under which the table folds a relation of `kind` after `delta`, None when the relation is affected."""
    if delta.change in NEVER_FOLDED:
        return None
    if delta.change == "doc_layout" and delta.shape == "callable" and not delta.decorated:
        return "no-code-change"
    if delta.change == "body" and delta.shape == "callable":
        if kind == "overrides":
            return "override-not-run"
        if kind in IMPORT_KINDS:
            return "imports-a-body"
        if kind == "type_use" and delta.typed and not runtime_annotation:
            return "declared-type"
    if delta.change == "signature_compatible" and delta.shape == "callable":
        if kind in ("calls", "instantiates"):
            return "check-the-default"
        if kind in IMPORT_KINDS:
            return "imports-a-body"
    if delta.change == "signature" and delta.shape == "callable" and kind in IMPORT_KINDS:
        return "imports-a-body"
    if delta.change in ("decorators", "class_body") and kind in IMPORT_KINDS:
        return "imports-a-body"
    if delta.change == "value" and delta.shape == "variable":
        if kind in IMPORT_KINDS:
            return "imports-a-body"
        if kind == "type_use" and delta.typed and not runtime_annotation:
            return "declared-type"
    return None


def runtime_annotation(snapshot: Snapshot, relation: Relation) -> bool:
    """A Python annotation something executes: a class body's field annotation, the signature of a decorated function,
    or any annotation in a file that imports a library which reads annotations at run time."""
    path = relation.from_key.rsplit("::", 1)[0]
    if relation.kind != "type_use" or not path.endswith(".py"):
        return False
    if RUNTIME_IMPORT.search(snapshot.source(path).decode("utf-8", "replace")):
        return True
    caller = next((d for d in snapshot.definitions(path) if d.key == relation.from_key), None)
    if caller is None:
        return False
    if caller.kind == "class":
        return True
    return caller.decorated and relation.site - 1 <= _body_row(snapshot, caller)


def _body_row(snapshot: Snapshot, caller) -> int:
    tree = snapshot.parsed(caller.path)
    node = (
        None
        if tree is None
        else tree.root_node.named_descendant_for_point_range(
            (caller.name_row, caller.name_col), (caller.name_row, caller.name_col + 1)
        )
    )
    while node is not None and node.type != "function_definition":
        node = node.parent
    body = None if node is None else node.child_by_field_name("body")
    return -1 if body is None else body.start_point.row


def mark_folding(relations: list[Relation], delta: Delta, snapshot: Snapshot) -> list[Relation]:
    return [replace(r, rule=fold_rule(delta, r.kind, runtime_annotation(snapshot, r))) for r in relations]
