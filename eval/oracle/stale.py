"""A removed definition is scored against head: what still names it there is a stale reference, what the range
changed so that it no longer names it cannot break (S3). A removal the engine pairs with a head definition of the
same kind and the same body, the name set aside, is a rename the answer states under its new name (S4)."""

from __future__ import annotations

import re
from dataclasses import replace

from changed import ChangedDefinition
from compare import CONSTRUCTORS
from model import MODULE, Definition, Relation
from truth import Snapshot


def _named_in(head: Snapshot, path: str, first: int, last: int, names: set[str], skip: set[int] = frozenset()) -> bool:
    """Some line of the span, a comment aside, names one of `names` as a whole word."""
    lines = head.source(path).decode("utf-8", "replace").split("\n")
    inert = head.inert_lines(path) | skip
    pattern = re.compile(rf"\b(?:{'|'.join(re.escape(n) for n in names)})\b")
    return any(pattern.search(lines[n - 1]) for n in range(first, min(last, len(lines)) + 1) if n not in inert)


def _ancestors(key: str) -> list[str]:
    path, _, qualname = key.rpartition("::")
    parts = qualname.split(".")
    return [f"{path}::{'.'.join(parts[:n])}" for n in range(len(parts), 0, -1)]


def still_named(relations: list[Relation], head: Snapshot, changed: list[ChangedDefinition], names: set[str]) -> list[Relation]:
    """The base users of a removed definition that still name it at head: the user is not changed by the range, or its
    head version (or its changed enclosing definition's) does."""
    by_base = {c.base.key: c for c in changed if c.base is not None}
    kept = []
    for relation in relations:
        path, _, qualname = relation.from_key.rpartition("::")
        if qualname == MODULE:
            lines = head.source(path).decode("utf-8", "replace").count("\n") + 1
            inside = {n for d in head.definitions(path) for n in range(d.start, d.end + 1)}
            if _named_in(head, path, 1, lines, names, inside):  # module code: the lines no definition holds
                kept.append(relation)
            continue
        owner = next((by_base[k] for k in _ancestors(relation.from_key) if k in by_base), None)
        if owner is None:
            kept.append(relation)  # nothing in the range touched the user
        elif owner.head is not None and _named_in(head, owner.head.path, owner.head.start, owner.head.end, names):
            kept.append(relation)
    return kept


def paired_head_definition(head: Snapshot, target: Definition, answered: list[str]) -> Definition | None:
    """The head definition the answer lists, of the target's kind, whose body is the target's once the name is set aside."""
    for key in answered:
        path = key.partition("::")[0]
        for candidate in head.definitions(path):
            if candidate.key == key and candidate.key != target.key and candidate.kind == target.kind:
                if candidate.shape_hash == target.shape_hash:
                    return candidate
    return None


def pairing(head: Snapshot, base: Snapshot, target: Definition, answered: list[str]) -> tuple[str, str] | None:
    """(the key of the head entry that answers for `target`, the key it is scored under): a constructor follows its class."""
    owner = target
    if target.name in CONSTRUCTORS and target.container:
        owner = next(
            (d for d in base.definitions(target.path) if d.kind == "class" and d.qualname == ".".join(target.container)), target
        )
    found = paired_head_definition(head, owner, answered)
    return None if found is None else (found.key, owner.key)


def renamed_answer(answer, scored_as: str, entry: str):
    """The answer with the entry of the paired head definition also filed under the removed definition's key."""
    if scored_as in answer.targets or entry not in answer.targets:
        return answer
    return replace(answer, targets={**answer.targets, scored_as: answer.targets[entry]})
