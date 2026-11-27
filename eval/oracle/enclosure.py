"""A changed definition the answer names at another grain is not a definition it lost.

The oracle lists a definition per AST node; the engine reports the unit a reader edits. Where the two grains differ the
answer names the change in a finer or a coarser definition than the oracle's:

* a local of a function the answer names is part of that function: the product holds no definition of its own for it;
* a container added or removed whole is named by naming it or by naming every member; the members and the shell go with it;
* a container whose changed lines all lie in members the answer names, or whose own tokens are the same on both sides,
  changed nothing beyond them: its members are judged one by one;
* an `origin: renamed` entry accounts for one removed definition of its file, the old name going unstated.

Nothing here names a definition the answer never mentions: a definition whose enclosing function, whole container or
member the answer also leaves out stays lost.
"""

from __future__ import annotations

import re
from collections.abc import Iterable
from typing import Protocol

import syntax
from answer import Answer
from changed import ChangedDefinition
from gitrepo import FileChange
from model import Definition, without_span
from truth import Snapshot

WHOLE = ("added", "removed")
FUNCTIONS = ("function", "method")
TOKEN = re.compile(r"\w+|[^\w\s]")

Scored = tuple[ChangedDefinition, dict | None]


class Range(Protocol):
    base: Snapshot
    head: Snapshot
    changes: list[FileChange]


def named_definitions(answer: Answer, unlisted: frozenset[str]) -> frozenset[str]:
    """The definitions the answer names anywhere, by key without the span that tells overloads apart."""
    return frozenset(without_span(key) for key in (*answer.targets, *unlisted, *answer.also_changed_named))


def _ancestors(definition: Definition) -> list[tuple[str, ...]]:
    """The qualified names around a definition, the nearest first."""
    return [definition.container[:n] for n in range(len(definition.container), 0, -1)]


def _key(path: str, qualname: Iterable[str]) -> str:
    return f"{path}::{'.'.join(qualname)}"


class _Cover:
    def __init__(self, scored: list[Scored], answer: Answer, unlisted: frozenset[str], scope: Range) -> None:
        self.scored, self.answer, self.scope = scored, answer, scope
        self.names = named_definitions(answer, unlisted)
        self.items = [item for item, _ in scored]
        self.by_name = {(item.target.path, item.target.qualname): i for i, item in enumerate(self.items)}
        self.covered: set[int] = set()

    def snapshot(self, item: ChangedDefinition) -> Snapshot:
        return self.scope.head if item.snapshot == "head" else self.scope.base

    def named(self, index: int) -> bool:
        return _key(self.items[index].target.path, [self.items[index].target.qualname]) in self.names or index in self.covered

    def local_of_a_named_function(self, item: ChangedDefinition) -> bool:
        target = item.target
        snapshot = self.snapshot(item)
        found = syntax.definitions(target.path, snapshot.source(target.path))
        functions = {(d.container, d.name) for d in found if d.kind in FUNCTIONS}
        return any(
            (outer[:-1], outer[-1]) in functions and _key(target.path, outer) in self.names for outer in _ancestors(target)
        )

    def whole_container_named(self, index: int) -> bool:
        target = self.items[index].target
        around = (self.by_name.get((target.path, ".".join(outer))) for outer in _ancestors(target))
        return any(
            above is not None and self.items[above].status == self.items[index].status and self.named(above) for above in around
        )

    def whole_with_every_member_named(self, index: int) -> bool:
        item = self.items[index]
        inside = (*item.target.container, item.target.name)
        members = [
            i
            for i, other in enumerate(self.items)
            if other.target.path == item.target.path and other.status == item.status and other.target.container == inside
        ]
        return bool(members) and all(self.named(i) for i in members)

    def own_changes_are_named_members(self, item: ChangedDefinition) -> bool:
        """Every changed line of the container lies in a member (the oracle's or one the answer names) or is a comment."""
        head, base = item.head, item.base
        assert head is not None and base is not None
        path = head.path
        defs = self.scope.head.definitions(path)
        inert = self.scope.head.inert_lines(path)
        spans = [(d.start, d.end) for d in defs if d is not head and head.start <= d.start and d.end <= head.end]
        spans += [
            (first, last)
            for first, last, removed in self.answer.named_spans.get(path, ())
            if not removed and head.start <= first and last <= head.end and (first, last) != (head.start, head.end)
        ]
        own = [r for r in _changed_ranges(self.scope.changes, path, head) if r[0] not in inert or r[1] != r[0]]
        if all(any(first <= lo and hi <= last for first, last in spans) for lo, hi in own):
            return True
        return _shell(self.scope.base, base) == _shell(self.scope.head, head)

    def sweep(self) -> None:
        lost = [i for i, (_, record) in enumerate(self.scored) if record]
        for i in lost:
            item = self.items[i]
            if self.local_of_a_named_function(item):
                self.covered.add(i)
            elif item.status == "modified" and item.head is not None and item.base is not None:
                if self.own_changes_are_named_members(item):
                    self.covered.add(i)
        changing = True
        while changing:
            changing = False
            for i in lost:
                item = self.items[i]
                if i not in self.covered and item.status in WHOLE:
                    if self.whole_container_named(i) or self.whole_with_every_member_named(i):
                        self.covered.add(i)
                        changing = True
        budget = dict(self.answer.renamed)
        for i in lost:
            path = self.items[i].target.path
            if i not in self.covered and self.items[i].status == "removed" and budget.get(path, 0) > 0:
                budget[path] -= 1
                self.covered.add(i)


def _changed_ranges(changes: list[FileChange], path: str, head: Definition) -> list[tuple[float, float]]:
    """The changed lines of `head` on the head side, each as (low, high); a pure deletion is the gap between two lines."""
    found: list[tuple[float, float]] = []
    for change in changes:
        if change.new_path != path:
            continue
        for hunk in change.hunks:
            if hunk.new_count > 0:
                found += [(n, n) for n in hunk.new_lines if head.start <= n <= head.end]
            elif head.start <= hunk.new_start and hunk.new_start + 1 <= head.end:
                found.append((hunk.new_start, hunk.new_start + 1))
    return found


def _shell(snapshot: Snapshot, definition: Definition) -> list[str]:
    """The tokens of a definition outside its members and its comments: the container itself."""
    path = definition.path
    inert = snapshot.inert_lines(path)
    inside = [
        (d.start, d.end)
        for d in snapshot.definitions(path)
        if d is not definition and definition.start <= d.start and d.end <= definition.end
    ]
    lines = snapshot.source(path).decode("utf-8", "replace").split("\n")
    kept = (
        lines[n - 1]
        for n in range(definition.start, min(definition.end, len(lines)) + 1)
        if n not in inert and not any(first <= n <= last for first, last in inside)
    )
    return TOKEN.findall(" ".join(kept))


def uncovered(scored: list[Scored], answer: Answer, unlisted: frozenset[str], scope: Range) -> list[dict]:
    """The records of `scored` whose definition the answer names at no grain: its silent candidates."""
    cover = _Cover(scored, answer, unlisted, scope)
    cover.sweep()
    return [record for i, (_, record) in enumerate(scored) if record and i not in cover.covered]
