"""Definitions and reference contexts from the oracle's own tree-sitter grammars.

A definition is a named declaration with its whole extent: decorators, the body and an `export` wrapper
included. The oracle maps hunks onto these extents, because a SCIP definition occurrence marks the name
only and a body-only edit never touches it.
"""

from __future__ import annotations

import hashlib
from functools import cache

import tree_sitter_javascript
import tree_sitter_python
import tree_sitter_typescript
from tree_sitter import Language, Node, Parser, Tree

from model import Definition, grammar_for

GRAMMARS = {
    "python": tree_sitter_python.language,
    "typescript": tree_sitter_typescript.language_typescript,
    "tsx": tree_sitter_typescript.language_tsx,
    "javascript": tree_sitter_javascript.language,
}
FUNCTION_VALUES = {"arrow_function", "function_expression", "function", "generator_function"}
TS_DECLARATIONS = {
    "function_declaration": "function",
    "generator_function_declaration": "function",
    "function_signature": "function",
    "class_declaration": "class",
    "abstract_class_declaration": "class",
    "interface_declaration": "interface",
    "type_alias_declaration": "type",
    "enum_declaration": "enum",
    "method_definition": "method",
    "abstract_method_signature": "method",
    "method_signature": "method",
    "public_field_definition": "property",
    "field_definition": "property",
    "property_signature": "property",
}
TS_SIGNATURE_FIELDS = ("type_parameters", "parameters", "return_type")
TS_NAMESPACES = {"internal_module", "module"}


@cache
def _parser(grammar: str) -> Parser:
    return Parser(Language(GRAMMARS[grammar]()))


def parse(path: str, source: bytes) -> Tree | None:
    grammar = grammar_for(path)
    return None if grammar is None else _parser(grammar).parse(source)


def kids(node: Node, named: bool = False) -> list[Node]:
    """Children by cursor: py-tree-sitter 0.26 crashes on `Node.children` for some large trees."""
    out: list[Node] = []
    cursor = node.walk()
    if cursor.goto_first_child():
        while True:
            child = cursor.node
            if child is not None and (child.is_named or not named):
                out.append(child)
            if not cursor.goto_next_sibling():
                break
    return out


def tokens(node: Node, source: bytes, blank_at: int = -1) -> list[bytes]:
    """Leaf texts in order, comments left out; the leaf starting at byte `blank_at` is replaced by a placeholder."""
    out: list[bytes] = []
    cursor = node.walk()
    while True:
        current = cursor.node
        if current is None:
            return out
        if current.type == "comment":
            pass
        elif current.child_count == 0:
            out.append(b"\0name" if current.start_byte == blank_at else source[current.start_byte : current.end_byte])
        elif cursor.goto_first_child():
            continue
        while True:
            if cursor.node is not None and cursor.node.id == node.id:
                return out
            if cursor.goto_next_sibling():
                break
            if not cursor.goto_parent():
                return out


def _digest(parts: list[bytes]) -> str:
    return hashlib.sha256(b"\0".join(parts)).hexdigest()[:16]


def _lines(node: Node) -> tuple[int, int]:
    end = node.end_point
    last = end.row + 1 if end.column > 0 or end.row == node.start_point.row else end.row
    return node.start_point.row + 1, last


class _Collector:
    def __init__(self, path: str, source: bytes) -> None:
        self.path = path
        self.source = source
        self.out: list[Definition] = []

    def text(self, node: Node) -> str:
        return self.source[node.start_byte : node.end_byte].decode("utf-8", "replace")

    def add(self, extent: Node, name_node: Node, kind: str, container: tuple[str, ...], sig_nodes: list[Node | None]) -> None:
        start, end = _lines(extent)
        signature = " ".join(b.decode("utf-8", "replace") for n in sig_nodes if n is not None for b in tokens(n, self.source))
        self.out.append(
            Definition(
                path=self.path,
                kind=kind,
                name=self.text(name_node),
                container=container,
                start=start,
                end=end,
                name_row=name_node.start_point.row,
                name_col=name_node.start_point.column,
                signature=signature,
                body_hash=_digest(tokens(extent, self.source)),
                shape_hash=_digest(tokens(extent, self.source, name_node.start_byte)),
                decorated=extent.type == "decorated_definition" or any(c.type == "decorator" for c in kids(extent)),
            )
        )


class _PythonWalker(_Collector):
    def run(self, root: Node) -> None:
        for child in kids(root, True):
            self.visit(child, (), "module")

    def visit(self, node: Node, container: tuple[str, ...], owner: str) -> None:
        if node.type == "decorated_definition":
            inner = node.child_by_field_name("definition")
            if inner is not None:
                self.definition(inner, node, container, owner)
        elif node.type in ("function_definition", "class_definition"):
            self.definition(node, node, container, owner)
        elif node.type == "expression_statement" and owner != "function":
            self.variable(node, container)
        else:
            for child in kids(node, True):
                self.visit(child, container, owner)

    def definition(self, node: Node, extent: Node, container: tuple[str, ...], owner: str) -> None:
        name = node.child_by_field_name("name")
        if name is None:
            return
        is_class = node.type == "class_definition"
        kind = "class" if is_class else ("method" if owner == "class" else "function")
        sig = (
            [node.child_by_field_name("superclasses")]
            if is_class
            else [node.child_by_field_name(f) for f in ("parameters", "return_type")]
        )
        self.add(extent, name, kind, container, sig)
        body = node.child_by_field_name("body")
        inner = (*container, self.text(name))
        for child in kids(body, True) if body is not None else ():
            self.visit(child, inner, "class" if is_class else "function")

    def variable(self, statement: Node, container: tuple[str, ...]) -> None:
        inner = kids(statement, True)
        assignment = inner[0] if inner else None
        if assignment is None or assignment.type != "assignment":
            return
        left = assignment.child_by_field_name("left")
        if left is not None and left.type == "identifier":
            self.add(statement, left, "variable", container, [assignment.child_by_field_name("type")])


class _ScriptWalker(_Collector):
    def run(self, root: Node) -> None:
        for child in kids(root, True):
            self.visit(child, (), False)

    def visit(self, node: Node, container: tuple[str, ...], in_function: bool) -> None:
        kind = node.type
        if kind == "export_statement":
            declaration = node.child_by_field_name("declaration")
            if declaration is not None:
                self.statement(declaration, node, container, in_function)
            return
        if kind in TS_NAMESPACES:
            name = node.child_by_field_name("name")
            body = node.child_by_field_name("body")
            if name is not None and body is not None:
                inner = (*container, self.text(name))
                self.children(body, inner, in_function)
            return
        if kind == "expression_statement":
            named = kids(node, True)
            first = named[0] if named else None
            if first is not None and first.type in TS_NAMESPACES:
                self.visit(first, container, in_function)
            else:
                self.member_assignment(node, container)
            return
        self.statement(node, node, container, in_function)

    def children(self, node: Node, container: tuple[str, ...], in_function: bool) -> None:
        for child in kids(node, True):
            self.visit(child, container, in_function)

    def statement(self, node: Node, extent: Node, container: tuple[str, ...], in_function: bool) -> None:
        kind = node.type
        if kind in TS_DECLARATIONS:
            self.declaration(node, extent, container)
        elif kind in ("lexical_declaration", "variable_declaration"):
            self.declarators(node, extent, container, in_function)
        else:
            self.children(node, container, in_function)

    def declaration(self, node: Node, extent: Node, container: tuple[str, ...]) -> None:
        name = node.child_by_field_name("name") or node.child_by_field_name("property")
        if name is None:
            return
        kind = TS_DECLARATIONS[node.type]
        heritage = [c for c in kids(node) if c.type in ("class_heritage", "extends_type_clause")]
        sig = heritage if kind in ("class", "interface") else [node.child_by_field_name(f) for f in TS_SIGNATURE_FIELDS]
        self.add(extent, name, kind, container, sig)
        inner = (*container, self.text(name))
        body = node.child_by_field_name("body")
        if body is not None:
            self.children(body, inner, kind not in ("class", "interface", "enum"))

    def declarators(self, node: Node, extent: Node, container: tuple[str, ...], in_function: bool) -> None:
        declarators = [c for c in kids(node, True) if c.type == "variable_declarator"]
        for declarator in declarators:
            name = declarator.child_by_field_name("name")
            value = declarator.child_by_field_name("value")
            if name is None or name.type != "identifier":
                continue
            is_function = value is not None and value.type in FUNCTION_VALUES
            if in_function and not is_function:
                continue
            span = extent if len(declarators) == 1 else declarator
            sig = [value.child_by_field_name(f) for f in ("parameters", "return_type")] if is_function and value else []
            self.add(span, name, "function" if is_function else "variable", container, sig)
            if value is not None:
                inner = (*container, self.text(name))
                self.children(value, inner, True)

    def member_assignment(self, statement: Node, container: tuple[str, ...]) -> None:
        """`app.listen = function () {}`: a definition by assignment, as plain JavaScript writes methods."""
        inner = kids(statement, True)
        assignment = inner[0] if inner else None
        if assignment is None or assignment.type != "assignment_expression":
            return
        left = assignment.child_by_field_name("left")
        right = assignment.child_by_field_name("right")
        if left is None or right is None or left.type != "member_expression" or right.type not in FUNCTION_VALUES:
            return
        prop = left.child_by_field_name("property")
        if prop is not None:
            sig = [right.child_by_field_name(f) for f in ("parameters", "return_type")]
            self.add(statement, prop, "function", container, sig)
            self.children(right, (*container, self.text(prop)), True)


def definitions(path: str, source: bytes) -> list[Definition]:
    """Every definition of the file, outermost first; empty for a file no grammar reads."""
    tree = parse(path, source)
    if tree is None:
        return []
    walker = _PythonWalker(path, source) if grammar_for(path) == "python" else _ScriptWalker(path, source)
    walker.run(tree.root_node)
    return sorted(walker.out, key=lambda d: (d.start, -d.end))


def reference_kind(root: Node, start: tuple[int, int], end: tuple[int, int], is_class: bool, grammar: str) -> str:
    """The syntactic relation of the identifier at `start`..`end` (0-based row, byte column) to what it names.

    SCIP marks no call: a callee position is `calls` (`instantiates` for a class), an argument or an assigned
    value is `passes_as_value`, a re-export clause is `reexports`, anything else `reads`.
    """
    node: Node | None = root.named_descendant_for_point_range(start, end)
    if node is None:
        return "reads"
    return _python_kind(node, is_class) if grammar == "python" else _script_kind(node, is_class)


def _same(a: Node | None, b: Node) -> bool:
    return a is not None and a == b


def _python_kind(node: Node, is_class: bool) -> str:
    while (
        node.parent is not None and node.parent.type == "attribute" and _same(node.parent.child_by_field_name("attribute"), node)
    ):
        node = node.parent
    parent = node.parent
    if parent is None:
        return "reads"
    if parent.type == "call" and _same(parent.child_by_field_name("function"), node):
        return "instantiates" if is_class else "calls"
    if parent.type == "decorator":
        return "calls"
    if parent.type == "argument_list" and parent.parent is not None and parent.parent.type == "call":
        return "passes_as_value"
    if parent.type == "keyword_argument" and _same(parent.child_by_field_name("value"), node):
        return "passes_as_value"
    if parent.type in ("assignment", "augmented_assignment") and _same(parent.child_by_field_name("right"), node):
        return "passes_as_value"
    if parent.type == "pair" and _same(parent.child_by_field_name("value"), node):
        return "passes_as_value"
    return "reads"


def _script_kind(node: Node, is_class: bool) -> str:
    while (
        node.parent is not None
        and node.parent.type == "member_expression"
        and _same(node.parent.child_by_field_name("property"), node)
    ):
        node = node.parent
    parent = node.parent
    if parent is None:
        return "reads"
    if parent.type == "new_expression" and _same(parent.child_by_field_name("constructor"), node):
        return "instantiates"
    if parent.type == "call_expression" and _same(parent.child_by_field_name("function"), node):
        return "instantiates" if is_class else "calls"
    if parent.type == "decorator":
        return "calls"
    if parent.type == "arguments":
        return "passes_as_value"
    if parent.type == "export_specifier":
        return "reexports" if _exports_from_module(parent) else "reads"
    if parent.type == "variable_declarator" and _same(parent.child_by_field_name("value"), node):
        return "passes_as_value"
    if parent.type in ("assignment_expression", "augmented_assignment_expression") and _same(
        parent.child_by_field_name("right"), node
    ):
        return "passes_as_value"
    if parent.type == "pair" and _same(parent.child_by_field_name("value"), node):
        return "passes_as_value"
    return "reads"


def _exports_from_module(node: Node) -> bool:
    current: Node | None = node
    while current is not None and current.type != "export_statement":
        current = current.parent
    return current is not None and current.child_by_field_name("source") is not None
