"""Truth from a SCIP index: which references reach a definition, and by what syntactic relation."""

from __future__ import annotations

from collections import OrderedDict, defaultdict
from pathlib import Path

import syntax
from model import MODULE, Definition, Relation, innermost
from scipindex import ROLE_IMPORT, ROLE_WRITE, Document, Index, Occurrence, byte_column

TREE_CACHE = 64


class Snapshot:
    """One indexed commit: its tree on disk, its index, and the definitions its files hold."""

    def __init__(self, tree: Path, index: Index, language: str) -> None:
        self.tree = tree
        self.index = index
        self.language = language
        self.documents: dict[str, Document] = {d.path: d for d in index.documents}
        self._sources: dict[str, bytes] = {}
        self._definitions: dict[str, list[Definition]] = {}
        self._trees: OrderedDict[str, syntax.Tree | None] = OrderedDict()
        self._implementers: dict[str, list[tuple[str, str]]] | None = None
        self._occurrences: dict[str, list[tuple[Document, Occurrence]]] | None = None

    def references(self, symbol: str) -> list[tuple[Document, Occurrence]]:
        """Every non-definition occurrence of `symbol`, from one pass over the index."""
        if self._occurrences is None:
            table: dict[str, list[tuple[Document, Occurrence]]] = defaultdict(list)
            for doc in self.index.documents:
                for occ in doc.occurrences:
                    if occ.symbol and not occ.is_definition:
                        table[occ.symbol].append((doc, occ))
            self._occurrences = table
        return self._occurrences.get(symbol, [])

    def source(self, path: str) -> bytes:
        if path not in self._sources:
            file = self.tree / path
            self._sources[path] = file.read_bytes() if file.is_file() and not file.is_symlink() else b""
        return self._sources[path]

    def definitions(self, path: str) -> list[Definition]:
        if path not in self._definitions:
            self._definitions[path] = syntax.definitions(path, self.source(path))
        return self._definitions[path]

    def parsed(self, path: str) -> syntax.Tree | None:
        if path in self._trees:
            self._trees.move_to_end(path)
            return self._trees[path]
        self._trees[path] = syntax.parse(path, self.source(path))
        if len(self._trees) > TREE_CACHE:
            self._trees.popitem(last=False)
        return self._trees[path]

    def position(self, doc: Document, occ: Occurrence) -> tuple[int, int, int]:
        lines = self.source(doc.path).split(b"\n")
        line = lines[occ.line] if occ.line < len(lines) else b""
        end_line = lines[occ.end_line] if occ.end_line < len(lines) else b""
        start = byte_column(line, occ.col, doc.position_encoding)
        end = byte_column(end_line, occ.end_col, doc.position_encoding)
        return start, occ.end_line, end

    def symbols_of(self, target: Definition) -> set[str]:
        """SCIP symbols whose definition occurrence sits on the name of `target`."""
        doc = self.documents.get(target.path)
        if doc is None:
            return set()
        found: set[str] = set()
        for occ in doc.occurrences:
            if occ.is_definition and occ.line == target.name_row and not occ.symbol.startswith("local "):
                if self.position(doc, occ)[0] == target.name_col:
                    found.add(occ.symbol)
        return found

    def enclosing(self, path: str, line: int) -> Definition | None:
        return innermost(self.definitions(path), line)

    def implementers(self) -> dict[str, list[tuple[str, str]]]:
        """related symbol -> [(path, symbol)] of the definitions that implement or override it."""
        if self._implementers is None:
            found: dict[str, list[tuple[str, str]]] = defaultdict(list)
            for doc in self.index.documents:
                for info in doc.symbols:
                    for related in info.implements:
                        found[related].append((doc.path, info.symbol))
            self._implementers = found
        return self._implementers

    def definition_of_symbol(self, path: str, symbol: str) -> Definition | None:
        doc = self.documents.get(path)
        if doc is None:
            return None
        for occ in doc.occurrences:
            if occ.is_definition and occ.symbol == symbol:
                start = self.position(doc, occ)[0]
                for d in self.definitions(path):
                    if d.name_row == occ.line and d.name_col == start:
                        return d
        return None


CONSTRUCTORS = {"__init__", "constructor"}


def _class_of(snapshot: Snapshot, target: Definition) -> Definition | None:
    if target.name not in CONSTRUCTORS or not target.container:
        return None
    for d in snapshot.definitions(target.path):
        if d.kind == "class" and d.qualname == ".".join(target.container):
            return d
    return None


def relations_to(snapshot: Snapshot, target: Definition) -> list[Relation]:
    """Every resolved relation from elsewhere in the snapshot to `target`; a constructor also takes its class's instantiations."""
    wanted: dict[str, str | None] = dict.fromkeys(snapshot.symbols_of(target))
    owner = _class_of(snapshot, target)
    if owner is not None:
        for symbol in snapshot.symbols_of(owner):
            wanted.setdefault(symbol, "instantiates")
    if not wanted:
        return []
    found: set[Relation] = set()
    for symbol, only in wanted.items():
        for doc, occ in snapshot.references(symbol):
            relation = _relation(snapshot, doc, occ, target, only == "instantiates")
            if relation is not None and (only is None or relation.kind == only):
                found.add(relation)
    for path, symbol in _implementing(snapshot, wanted):
        implementer = snapshot.definition_of_symbol(path, symbol)
        if implementer is not None and implementer.key != target.key:
            found.add(Relation(implementer.key, target.key, "overrides", implementer.start))
    return sorted(found, key=lambda r: (r.from_key, r.site, r.kind))


def _implementing(snapshot: Snapshot, wanted: dict[str, str | None]) -> list[tuple[str, str]]:
    table = snapshot.implementers()
    return [pair for symbol, only in wanted.items() if only is None for pair in table.get(symbol, [])]


def _relation(snapshot: Snapshot, doc: Document, occ: Occurrence, target: Definition, names_class: bool) -> Relation | None:
    line = occ.line + 1
    caller = snapshot.enclosing(doc.path, line)
    if caller is not None and caller.key == target.key:
        return None
    from_key = caller.key if caller is not None else f"{doc.path}::{MODULE}"
    return Relation(from_key, target.key, _kind(snapshot, doc, occ, target, names_class), line)


def _kind(snapshot: Snapshot, doc: Document, occ: Occurrence, target: Definition, names_class: bool) -> str:
    if occ.roles & ROLE_IMPORT:
        return "imports"
    if occ.roles & ROLE_WRITE:
        return "writes"
    tree = snapshot.parsed(doc.path)
    grammar = syntax.grammar_for(doc.path)
    if tree is None or grammar is None:
        return "reads"
    start, end_row, end = snapshot.position(doc, occ)
    return syntax.reference_kind(
        tree.root_node,
        (occ.line, start),
        (end_row, end),
        target.kind == "class" or names_class,
        "python" if grammar == "python" else "script",
    )
