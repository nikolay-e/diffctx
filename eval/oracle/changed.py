"""Changed definitions by AST containment: which definitions a commit touches, body-only edits included.

A hunk is mapped to the innermost enclosing definition on head for added and modified lines and on base for
deleted lines. A definition that reappears elsewhere or under another name with the same body was moved or
renamed, which is no removal; so is one that reappears edited, when its name has a single new home.
"""

from __future__ import annotations

from collections import defaultdict
from collections.abc import Callable
from dataclasses import dataclass

from gitrepo import FileChange, Hunk
from model import Definition, grammar_for, innermost

DefinitionsOf = Callable[[str], list[Definition]]
InertLinesOf = Callable[[str], set[int]]


@dataclass(frozen=True)
class ChangedDefinition:
    status: str  # modified | added | removed | moved | renamed | layout
    head: Definition | None
    base: Definition | None

    @property
    def target(self) -> Definition:
        found = self.head or self.base
        assert found is not None
        return found

    @property
    def snapshot(self) -> str:
        return "head" if self.head is not None else "base"


def _contains_definitions(defs: list[Definition], outer: Definition) -> bool:
    return any(d is not outer and outer.start <= d.start and d.end <= outer.end for d in defs)


def _touched(defs: list[Definition], lines: range, boundary: int | None, inert: set[int]) -> list[Definition]:
    """Definitions holding a changed line; `boundary` is the pure-deletion point on head: the gap between two lines.

    A blank or comment-only line that falls directly in a definition holding others touches nothing: the blank
    line and the doc comment of an added or removed member are not a change of the class around it. Nor does a
    pure deletion there; the base side names what was deleted.
    """
    if boundary is not None:
        inside = [d for d in defs if d.start <= boundary and d.end >= boundary + 1]
        found = innermost(inside, boundary)
        return [found] if found and not _contains_definitions(defs, found) else []
    candidates = [d for d in defs if lines and d.start <= lines[-1] and d.end >= lines[0]]
    seen: dict[tuple[str, int, int], Definition] = {}
    for line in lines:
        d = innermost(candidates, line)
        if d is not None and not (line in inert and _contains_definitions(defs, d)):
            seen[(d.path, d.start, d.end)] = d
    return list(seen.values())


def _head_touched(defs: list[Definition], hunk: Hunk, inert: set[int]) -> list[Definition]:
    if hunk.new_count > 0:
        return _touched(defs, hunk.new_lines, None, inert)
    return _touched(defs, range(0), hunk.new_start, inert)


def _identity_index(defs: list[Definition]) -> dict[tuple[str, tuple[str, ...], str], list[Definition]]:
    index: dict[tuple[str, tuple[str, ...], str], list[Definition]] = defaultdict(list)
    for d in defs:
        index[(d.path, d.container, d.name)].append(d)
    return index


def _nth(candidates: list[Definition], ordinal: int) -> Definition:
    return candidates[min(ordinal, len(candidates) - 1)]


def changed_definitions(
    changes: list[FileChange],
    base_defs: DefinitionsOf,
    head_defs: DefinitionsOf,
    base_inert: InertLinesOf,
    head_inert: InertLinesOf,
) -> list[ChangedDefinition]:
    code = [c for c in changes if grammar_for(c.new_path or c.old_path or "") is not None]
    head_all: list[Definition] = []
    base_all: list[Definition] = []
    touched_head: dict[tuple[str, int, int], Definition] = {}
    touched_base: dict[tuple[str, int, int], Definition] = {}
    moved_to = {c.old_path: c.new_path for c in code if c.old_path and c.new_path}
    moved_from = {c.new_path: c.old_path for c in code if c.old_path and c.new_path}
    for change in code:
        if change.new_path:
            defs = head_defs(change.new_path)
            inert = head_inert(change.new_path)
            head_all += defs
            for hunk in change.hunks:
                for d in _head_touched(defs, hunk, inert):
                    touched_head[(d.path, d.start, d.end)] = d
        if change.old_path:
            defs = base_defs(change.old_path)
            inert = base_inert(change.old_path)
            base_all += defs
            for hunk in change.hunks:
                if hunk.old_count > 0:
                    for d in _touched(defs, hunk.old_lines, None, inert):
                        touched_base[(d.path, d.start, d.end)] = d
    head_index = _identity_index(head_all)
    base_index = _identity_index(base_all)
    fresh = [h for h in head_all if (moved_from.get(h.path, h.path), h.container, h.name) not in base_index]
    out: list[ChangedDefinition] = []
    used: set[tuple[str, int, int]] = set()
    for b in touched_base.values():
        match = _match_forward(b, moved_to, base_index, head_index, fresh)
        out.append(match)
        if match.head is not None:
            used.add((match.head.path, match.head.start, match.head.end))
    for key, h in touched_head.items():
        if key in used:
            continue
        old_path = moved_from.get(h.path, h.path)
        siblings = base_index.get((old_path, h.container, h.name))
        if siblings:
            ordinal = head_index[(h.path, h.container, h.name)].index(h)
            out.append(_pair(_nth(siblings, ordinal), h))
        else:
            out.append(ChangedDefinition("added", h, None))
    return sorted(out, key=lambda c: (c.target.path, c.target.start, c.target.end))


def _pair(b: Definition, h: Definition) -> ChangedDefinition:
    same = b.body_hash == h.body_hash and b.signature == h.signature
    return ChangedDefinition("layout" if same else "modified", h, b)


def _match_forward(
    b: Definition,
    moved_to: dict[str, str],
    base_index: dict[tuple[str, tuple[str, ...], str], list[Definition]],
    head_index: dict[tuple[str, tuple[str, ...], str], list[Definition]],
    fresh: list[Definition],
) -> ChangedDefinition:
    """What became of a base definition touched by the range. `fresh` holds the head definitions no base definition stands
    behind: a move or a rename lands on one of those, never on a definition the base already had under its own name."""
    here = head_index.get((moved_to.get(b.path, b.path), b.container, b.name))
    if here:
        ordinal = base_index[(b.path, b.container, b.name)].index(b)
        return _pair(b, _nth(here, ordinal))
    same_body = [h for h in fresh if h.name == b.name and h.signature == b.signature and h.body_hash == b.body_hash]
    if same_body:
        return ChangedDefinition("moved", same_body[0], b)
    renamed = [h for h in fresh if h.name != b.name and h.signature == b.signature and h.shape_hash == b.shape_hash]
    if len(renamed) == 1:
        return ChangedDefinition("renamed", renamed[0], b)
    relocated = [h for h in fresh if h.name == b.name and h.container == b.container and h.kind == b.kind]
    if len(relocated) == 1:  # edited on the way to its one new home
        return ChangedDefinition("moved", relocated[0], b)
    return ChangedDefinition("removed", None, b)
