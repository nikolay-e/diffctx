"""Plain data the other modules share; nothing here needs a parser."""

from __future__ import annotations

import re
from dataclasses import dataclass, replace

MODULE = "<module>"
UNCAPPED_BUDGET = 200_000  # `--budget` far above any answer the engine builds: the cap never bites

SUFFIX_GRAMMAR = {
    ".py": "python",
    ".ts": "typescript",
    ".mts": "typescript",
    ".cts": "typescript",
    ".tsx": "tsx",
    ".js": "javascript",
    ".jsx": "javascript",
    ".mjs": "javascript",
    ".cjs": "javascript",
    ".rs": "rust",
    ".go": "go",
    ".kt": "kotlin",
    ".java": "java",
}


@dataclass(frozen=True)
class Definition:
    path: str
    kind: str
    name: str
    container: tuple[str, ...]
    start: int  # first line, 1-based
    end: int  # last line, inclusive
    name_row: int  # 0-based row of the name
    name_col: int  # byte column of the name
    signature: str
    body_hash: str  # hash of the non-comment tokens: formatting and comments do not move it
    shape_hash: str  # the same with the name blanked, so a rename keeps it
    decorated: bool = False
    overloaded: bool = False  # another definition of the file has this container and name: the span tells them apart

    @property
    def qualname(self) -> str:
        return ".".join((*self.container, self.name))

    @property
    def key(self) -> str:
        return f"{self.path}::{self.qualname}" + (f"@{self.start}-{self.end}" if self.overloaded else "")

    @property
    def span(self) -> int:
        return self.end - self.start


SPAN_MARK = re.compile(r"@\d+-\d+$")


def without_span(key: str) -> str:
    """The key of a definition without the span that tells its overloads apart."""
    return SPAN_MARK.sub("", key)


def mark_overloads(defs: list[Definition]) -> list[Definition]:
    """Definitions of one file that share container and name are keyed by their spans: the engine answers per definition."""
    counts: dict[tuple[tuple[str, ...], str], int] = {}
    for d in defs:
        counts[(d.container, d.name)] = counts.get((d.container, d.name), 0) + 1
    return [replace(d, overloaded=True) if counts[(d.container, d.name)] > 1 else d for d in defs]


def grammar_for(path: str) -> str | None:
    dot = path.rfind(".")
    return SUFFIX_GRAMMAR.get(path[dot:].lower()) if dot >= 0 else None


def innermost(defs: list[Definition], line: int) -> Definition | None:
    """The smallest definition whose extent holds `line` (1-based)."""
    best: Definition | None = None
    for d in defs:
        if d.start <= line <= d.end and (best is None or d.span <= best.span):
            best = d
    return best


@dataclass(frozen=True)
class Relation:
    from_key: str  # path::qualname, or path::<module> for a reference outside every definition
    to_key: str
    kind: str
    site: int  # 1-based line of the reference in the `from` file
    resolution: str = "resolved"
    in_diff: bool = False
    rule: str | None = None  # the folding rule (Rev 12.1) under which the change cannot affect it; None: affected
    moved: tuple[str, ...] = ()  # the rulings (R8...) that changed this row's kind from what the syntax alone gave
