"""Plain data the other modules share; nothing here needs a parser."""

from __future__ import annotations

from dataclasses import dataclass

MODULE = "<module>"

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

    @property
    def qualname(self) -> str:
        return ".".join((*self.container, self.name))

    @property
    def key(self) -> str:
        return f"{self.path}::{self.qualname}"

    @property
    def span(self) -> int:
        return self.end - self.start


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
