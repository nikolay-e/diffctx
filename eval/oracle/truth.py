"""Truth from a SCIP index: which references reach a definition, and by what syntactic relation."""

from __future__ import annotations

import re
from collections import OrderedDict, defaultdict
from pathlib import Path, PurePosixPath

import syntax
from filters import Filters
from model import MODULE, Definition, Relation, innermost, mark_overloads
from scipindex import ROLE_IMPORT, ROLE_WRITE, Document, Index, Occurrence, byte_column

TREE_CACHE = 64
# `rust-analyzer cargo shop 1.0.0 shop/impl#[`Raw<'_>`][Executor]run().`: the package, the type and the trait named by an impl's member
RUST_IMPL_MEMBER = re.compile(
    r"^(?P<package>(?:\S+ ){3}\S+) (?:\S*/)?impl#\[(?:`[^`]*`|[^\]]*)\]\[(?P<trait>[^\]<]+)[^\]]*\](?P<member>.+)$"
)
CLASS_KINDS = {7, 49}  # scip.proto SymbolInformation.Kind: Class, Struct


def _scope(symbol: str, path: str) -> str:
    """Local symbols are numbered per document, so two documents' `local 3` are different things."""
    return f"{path}\0{symbol}" if symbol.startswith("local ") else symbol


def _inside_repository(path: str) -> bool:
    return not PurePosixPath(path).is_absolute() and ".." not in PurePosixPath(path).parts


class Snapshot:
    """One indexed commit: its tree on disk, its index, and the definitions its files hold."""

    def __init__(self, tree: Path, index: Index, language: str) -> None:
        self.tree = tree
        # A document outside the repository (the Go build cache's generated test main is indexed as `../../go/build/..`)
        # is no source file of it: an edge from there is not truth.
        index.documents = [d for d in index.documents if _inside_repository(d.path)]
        self.index = index
        self.language = language
        self.documents: dict[str, Document] = {d.path: d for d in index.documents}
        self._sources: dict[str, bytes] = {}
        self._definitions: dict[str, list[Definition]] = {}
        self._trees: OrderedDict[str, syntax.Tree | None] = OrderedDict()
        self._implementers: dict[str, list[tuple[str, str]]] | None = None
        self._rust_members: dict[tuple[str, str], set[str]] | None = None
        self._occurrences: dict[str, list[tuple[Document, Occurrence]]] | None = None
        self._kinds: dict[str, int] | None = None
        self.filters = Filters(self)

    def is_class_symbol(self, symbol: str) -> bool:
        """A class by the index: scip-typescript says Class or Struct, scip-python says nothing and marks types with `#`."""
        if self._kinds is None:
            self._kinds = {s.symbol: s.kind for d in self.index.documents for s in d.symbols}
        kind = self._kinds.get(symbol, 0)
        return kind in CLASS_KINDS or (kind == 0 and symbol.endswith("#"))

    def references(self, symbol: str, path: str) -> list[tuple[Document, Occurrence]]:
        """Every non-definition occurrence of `symbol` (a local symbol only within `path`), from one pass over the index."""
        if self._occurrences is None:
            table: dict[str, list[tuple[Document, Occurrence]]] = defaultdict(list)
            for doc in self.index.documents:
                for occ in doc.occurrences:
                    if occ.symbol and not occ.is_definition:
                        table[_scope(occ.symbol, doc.path)].append((doc, occ))
            self._occurrences = table
        return self._occurrences.get(_scope(symbol, path), [])

    def source(self, path: str) -> bytes:
        if path not in self._sources:
            file = self.tree / path
            self._sources[path] = file.read_bytes() if file.is_file() and not file.is_symlink() else b""
        return self._sources[path]

    def inert_lines(self, path: str) -> set[int]:
        """1-based numbers of the lines of `path` that hold nothing but whitespace or comment."""
        source = self.source(path)
        found = {n for n, line in enumerate(source.split(b"\n"), 1) if not line.strip()}
        tree = self.parsed(path)
        return found if tree is None else found | syntax.comment_only_lines(tree.root_node, source)

    def definitions(self, path: str) -> list[Definition]:
        """The definitions of `path` that are not local to a function body: a local belongs to its enclosing definition (R18)."""
        if path not in self._definitions:
            found = syntax.definitions(path, self.source(path))
            self._definitions[path] = mark_overloads([d for d in found if not self._is_local(d, found)])
        return self._definitions[path]

    def _is_local(self, definition: Definition, siblings: list[Definition]) -> bool:
        symbols = self.symbols_of(definition)
        script = syntax.grammar_for(definition.path) in syntax.SCRIPT_GRAMMARS
        if symbols and all(symbol.startswith("local ") for symbol in symbols):
            return not script or not self._leaves_its_function(definition, siblings)
        return definition.path.endswith(".py") and self._inside_a_function(definition, siblings)

    def _enclosing_function(self, definition: Definition, siblings: list[Definition]) -> tuple[Definition, Definition] | None:
        """The outermost function the definition is nested in, and the definition of it that holds the one asked about."""
        if not definition.container:
            return None
        chain = [
            next(
                (d for d in siblings if d.container == definition.container[: n - 1] and d.name == definition.container[n - 1]),
                None,
            )
            for n in range(1, len(definition.container) + 1)
        ]
        outer = next((i for i, d in enumerate(chain) if d is not None and d.kind in ("function", "method")), None)
        if outer is None or chain[outer] is None:
            return None
        return chain[outer], (chain[outer + 1] if outer + 1 < len(chain) else definition)

    def _leaves_its_function(self, definition: Definition, siblings: list[Definition]) -> bool:
        """A local TypeScript function or member assignment the enclosing function returns or stores outward is a
        definition of its own, whatever the index says of its symbol: the edits inside it are not the function's."""
        found = self._enclosing_function(definition, siblings)
        tree = self.parsed(definition.path)
        if found is None or tree is None or found[1] is None:
            return False
        function, top = found
        named = tree.root_node.named_descendant_for_point_range(
            (function.name_row, function.name_col), (function.name_row, function.name_col + 1)
        )
        node = None if named is None else syntax.script_function_of(named)
        member = tree.root_node.named_descendant_for_point_range((top.name_row, top.name_col), (top.name_row, top.name_col + 1))
        subject = top.name
        if member is not None and member.parent is not None and member.parent.type == "member_expression":
            root = member.parent  # `got.extend = ...` is the enclosing function's `got` handed on
            while (
                root.child_by_field_name("object") is not None and root.child_by_field_name("object").type == "member_expression"
            ):
                root = root.child_by_field_name("object")
            holder = root.child_by_field_name("object")
            subject = top.name if holder is None else holder.text.decode("utf-8", "replace")
        return node is not None and syntax.script_escapes(node, subject, self.source(definition.path))

    def _inside_a_function(self, definition: Definition, siblings: list[Definition]) -> bool:
        """A Python definition nested in a function that does not leave it, whatever the index calls its symbol (R18).

        scip-python names a nested function with a full descriptor and not `local N`; the syntax decides instead.
        """
        found = self._enclosing_function(definition, siblings)
        tree = self.parsed(definition.path)
        if found is None or tree is None:
            return False
        function, top = found
        node = tree.root_node.named_descendant_for_point_range(
            (function.name_row, function.name_col), (function.name_row, function.name_col + 1)
        )
        while node is not None and node.type != "function_definition":
            node = node.parent
        return node is not None and top is not None and not syntax.python_escapes(node, top.name, self.source(definition.path))

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
            if occ.is_definition and occ.line == target.name_row and occ.symbol:
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
            for symbol, path in self._rust_impl_members():
                for related in self._trait_members(symbol):
                    found[related].append((path, symbol))
            self._implementers = found
        return self._implementers

    def _rust_impl_members(self) -> list[tuple[str, str]]:
        """rust-analyzer emits no implementation relationship; the name of an impl's member carries the trait it implements."""
        return [
            (info.symbol, doc.path)
            for doc in self.index.documents
            for info in doc.symbols
            if info.symbol.startswith("rust-analyzer ") and RUST_IMPL_MEMBER.match(info.symbol)
        ]

    def _trait_members(self, impl_member: str) -> list[str]:
        """The trait's own member, when the index defines exactly one by that trait and member name in the package."""
        matched = RUST_IMPL_MEMBER.match(impl_member)
        if matched is None:
            return []
        if self._rust_members is None:
            table: dict[tuple[str, str], set[str]] = defaultdict(set)
            for doc in self.index.documents:
                for occ in doc.occurrences:
                    if occ.is_definition and occ.symbol.startswith("rust-analyzer ") and "impl#" not in occ.symbol:
                        *package, descriptor = occ.symbol.split(" ", 4)
                        table[(" ".join(package), descriptor.rpartition("/")[2])].add(occ.symbol)
            self._rust_members = table
        found = self._rust_members.get((matched["package"], f"{matched['trait']}#{matched['member']}"), set())
        return list(found) if len(found) == 1 else []

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
        for doc, occ in snapshot.references(symbol, target.path):
            relation = _relation(snapshot, doc, occ, target, only == "instantiates", snapshot.is_class_symbol(symbol))
            if relation is not None and (only is None or relation.kind == only):
                found.add(relation)
            if only is None:
                found.update(_through_aliases(snapshot, doc, occ, target))
    for path, symbol in _implementing(snapshot, wanted):
        implementer = snapshot.definition_of_symbol(path, symbol)
        if implementer is not None and implementer.key != target.key:
            found.add(Relation(implementer.key, target.key, "overrides", implementer.start))
    return sorted(found, key=lambda r: (r.from_key, r.site, r.kind))


def _destructured_binding(snapshot: Snapshot, doc: Document, occ: Occurrence) -> Occurrence | None:
    """The definition occurrence of the name `const {m} = X` or `const {m: alias} = X` binds, when `occ` is that
    pattern's `m`: the member the pattern takes, read off the syntax."""
    tree = snapshot.parsed(doc.path)
    if tree is None or syntax.grammar_for(doc.path) not in syntax.SCRIPT_GRAMMARS:
        return None
    start, end_row, end = snapshot.position(doc, occ)
    node = tree.root_node.named_descendant_for_point_range((occ.line, start), (end_row, end))
    if node is None or node.parent is None:
        return None
    pattern, bound = node.parent, node
    if node.type == "property_identifier" and pattern.type == "pair_pattern":
        bound = pattern.child_by_field_name("value")
        pattern = pattern.parent
        if bound is not None and bound.type == "assignment_pattern":
            bound = bound.child_by_field_name("left")
    elif node.type not in ("shorthand_property_identifier_pattern", "object_assignment_pattern"):
        return None
    holder = None if pattern is None else pattern.parent
    if pattern is None or pattern.type != "object_pattern" or holder is None or holder.type != "variable_declarator":
        return None
    if bound is None:
        return None
    row, col = bound.start_point
    return next((o for o in doc.occurrences if o.is_definition and o.line == row and snapshot.position(doc, o)[0] == col), None)


def _through_aliases(snapshot: Snapshot, doc: Document, occ: Occurrence, target: Definition) -> list[Relation]:
    """A call through a name destructured from the member's owner, `const {m} = X; m(...)`: SCIP binds it to the local
    constant, so the uses of the constant are uses of the member the pattern took."""
    binding = _destructured_binding(snapshot, doc, occ)
    if binding is None:
        return []
    found = []
    for user_doc, user in snapshot.references(binding.symbol, doc.path):
        line = user.line + 1
        caller = snapshot.enclosing(user_doc.path, line)
        if caller is not None and caller.key == target.key:
            continue
        from_key = caller.key if caller is not None else f"{user_doc.path}::{MODULE}"
        kind, moved = _kind(snapshot, user_doc, user, target, False)
        found.append(Relation(from_key, target.key, kind, line, moved=(*moved, "alias")))
    return found


def _implementing(snapshot: Snapshot, wanted: dict[str, str | None]) -> list[tuple[str, str]]:
    table = snapshot.implementers()
    return [pair for symbol, only in wanted.items() if only is None for pair in table.get(symbol, [])]


def _relation(
    snapshot: Snapshot, doc: Document, occ: Occurrence, target: Definition, names_class: bool, indexed_class: bool
) -> Relation | None:
    line = occ.line + 1
    caller = snapshot.enclosing(doc.path, line)
    if caller is not None and caller.key == target.key:
        return None
    if snapshot.filters.rejects(doc, occ, target, caller):
        return None
    from_key = caller.key if caller is not None else f"{doc.path}::{MODULE}"
    kind, moved = _kind(snapshot, doc, occ, target, names_class)
    if kind == "calls" and indexed_class:
        kind, moved = "instantiates", (*moved, "R10")  # the callee is a class by the index, whatever the AST made of its target
    return Relation(from_key, target.key, kind, line, moved=moved)


def _kind(
    snapshot: Snapshot, doc: Document, occ: Occurrence, target: Definition, names_class: bool
) -> tuple[str, tuple[str, ...]]:
    if occ.roles & ROLE_IMPORT:
        return "imports", ()
    if occ.roles & ROLE_WRITE:
        return "writes", ()
    tree = snapshot.parsed(doc.path)
    grammar = syntax.grammar_for(doc.path)
    if tree is None or grammar is None:
        return "reads", ()
    start, end_row, end = snapshot.position(doc, occ)
    return syntax.ruled_kind(
        tree.root_node,
        (occ.line, start),
        (end_row, end),
        target.kind == "class" or names_class,
        grammar,
    )
