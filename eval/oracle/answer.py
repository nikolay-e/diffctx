"""What an engine answered about a change, as the scorer reads it: one shape, whatever the schema of the document.

`parse_answer` takes a `diffctx.impact.v1` document (the frozen adapter in `answer_v1.py`, which reads prose and
`path::symbol (lines)` strings) or a `diffctx.impact.v2` one (`answer_v2.py`, structured gaps and definition references).
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass, field

Resolver = Callable[[str, str | None, str | None], str]
# The kinds a row may state (schema v1 and v2): the vocabulary of the truth's relations.
ROW_KINDS = ("calls", "instantiates", "passes_as_value", "writes", "reads", "type_use", "overrides", "reexports", "imports")
V1, V2 = "diffctx.impact.v1", "diffctx.impact.v2"


@dataclass
class AnswerCaller:
    from_key: str
    evidence: str
    relation: str  # call | reference | reexports | also_changed
    kind: str | None = None  # the row's own kind (schema v1, a precise cell only): calls, type_use, ...
    folded_by: str | None = None  # the folding rule the engine applied to this row (Rev 12.1): it would not list it


@dataclass
class TargetAnswer:
    key: str
    callers: list[AnswerCaller] = field(default_factory=list)
    unresolved: str | None = None
    missed_in: list[str] = field(default_factory=list)
    gapped: set[str] = field(default_factory=set)  # the search groups (calls, reads) whose coverage names a gap (v2)
    callers_omitted: int = 0  # callers the answer cap dropped from this symbol: its list is a lower bound
    entry: str | None = None  # how the symbol is entered ("run as a script"): callers are not what reaches it


@dataclass
class Answer:
    targets: dict[str, TargetAnswer]
    limits: list[str]
    unanalysed: set[str]
    empty: bool
    # Definitions the cap shed whose names the answer also left out ("and N more"): any target it does not
    # list may be one of them.
    unnamed_omitted: int = 0
    # Callers the cap dropped (the answer's top-level `truncated`) that no entry attributes to a symbol, as 1.20.0's
    # answers do: every listed list may be one of the cut ones.
    truncated_unattributed: int = 0
    truncated: int = 0  # callers the cap dropped, wherever they were (the answer's top-level count)


def parse_answer(data: dict, resolve: Resolver) -> Answer:
    """The answer in the shape the scorer reads; the schema names the reader, a document without one is v1."""
    schema = data.get("schema", V1)
    if schema == V2:
        from answer_v2 import parse_v2

        return parse_v2(data, resolve)
    if schema == V1:
        from answer_v1 import parse_v1

        return parse_v1(data, resolve)
    raise ValueError(f"an impact answer of schema {schema!r}: this oracle reads {V1} and {V2}")
