"""Definitions and reference contexts from the oracle's own tree-sitter grammars.

A definition is a named declaration with its whole extent: decorators, the body and an `export` wrapper
included. The oracle maps hunks onto these extents, because a SCIP definition occurrence marks the name
only and a body-only edit never touches it.
"""

from __future__ import annotations

import hashlib
from functools import cache

import tree_sitter_go
import tree_sitter_java
import tree_sitter_javascript
import tree_sitter_kotlin
import tree_sitter_python
import tree_sitter_rust
import tree_sitter_typescript
from tree_sitter import Language, Node, Parser, Tree

from model import Definition, grammar_for

SCRIPT_GRAMMARS = ("typescript", "tsx", "javascript")
GRAMMARS = {
    "python": tree_sitter_python.language,
    "typescript": tree_sitter_typescript.language_typescript,
    "tsx": tree_sitter_typescript.language_tsx,
    "javascript": tree_sitter_javascript.language,
    "rust": tree_sitter_rust.language,
    "go": tree_sitter_go.language,
    "java": tree_sitter_java.language,
    "kotlin": tree_sitter_kotlin.language,
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
COMMENT_TYPES = {"comment", "line_comment", "block_comment", "multiline_comment"}
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


def _named_as_a_value(node: Node, name: str, source: bytes, script: bool = False) -> bool:
    """`name` occurs under `node` other than as the callee of a call: `inner` escapes, `inner()` only runs."""
    call = "call_expression" if script else "call"
    names = ("identifier", "shorthand_property_identifier") if script else ("identifier",)
    stack = [node]
    while stack:
        current = stack.pop()
        parent = current.parent
        is_callee = parent is not None and parent.type == call and parent.child_by_field_name("function") == current
        if (
            current.type in names
            and not is_callee
            and source[current.start_byte : current.end_byte].decode("utf-8", "replace") == name
        ):
            return True
        stack.extend(kids(current))
    return False


def python_escapes(function: Node, name: str, source: bytes) -> bool:
    """A name defined inside a Python function leaves it when the function returns or yields it, or assigns it to an
    attribute or an item (`self.cb = cb`). Passing it as an argument, decorating with it or calling it does not."""
    stack = [function.child_by_field_name("body")]
    while stack:
        node = stack.pop()
        if node is None or node.type in ("function_definition", "class_definition", "decorated_definition", "lambda"):
            continue  # what happens inside a nested definition is that definition's own business
        if node.type in ("return_statement", "yield") and _named_as_a_value(node, name, source):
            return True
        if node.type in ("assignment", "augmented_assignment"):
            left, right = node.child_by_field_name("left"), node.child_by_field_name("right")
            if (
                left is not None
                and right is not None
                and left.type in ("attribute", "subscript")
                and _named_as_a_value(right, name, source)
            ):
                return True
        stack.extend(kids(node, True))
    return False


SCRIPT_FUNCTIONS = {
    "function_declaration",
    "generator_function_declaration",
    "function_expression",
    "arrow_function",
    "method_definition",
    "function",
}


def script_function_of(name: Node) -> Node | None:
    """The function a TypeScript definition's name names: a declarator's value, or the declaration around it."""
    node: Node | None = name
    while node is not None:
        parent = node.parent
        if parent is not None and parent.type == "variable_declarator" and _same(parent.child_by_field_name("name"), node):
            return unwrapped(parent.child_by_field_name("value"))
        if node.type in SCRIPT_FUNCTIONS:
            return node
        node = parent
    return None


def script_escapes(function: Node, name: str, source: bytes) -> bool:
    """A name defined inside a TypeScript or JavaScript function leaves it when the function returns it, or stores it as a
    member of something (`got.paginate = paginateEach`). Calling it or passing it as an argument does not."""
    stack = [function.child_by_field_name("body")]
    while stack:
        node = stack.pop()
        if node is None or node.type in SCRIPT_FUNCTIONS or node.type in ("class_declaration", "class"):
            continue  # what happens inside a nested definition is that definition's own business
        if node.type == "return_statement" and _named_as_a_value(node, name, source, script=True):
            return True
        if node.type == "assignment_expression":
            left, right = node.child_by_field_name("left"), node.child_by_field_name("right")
            if (
                left is not None
                and right is not None
                and left.type == "member_expression"
                and _named_as_a_value(right, name, source, script=True)
            ):
                return True
        stack.extend(kids(node, True))
    return False


def comment_only_lines(root: Node, source: bytes) -> set[int]:
    """1-based lines holding nothing but comment (and whitespace): a doc comment's every line counts."""
    lines = source.split(b"\n")
    found: set[int] = set()
    stack = [root]
    while stack:
        node = stack.pop()
        if node.type not in COMMENT_TYPES:
            stack.extend(kids(node))
            continue
        (first, first_col), (last, last_col) = node.start_point, node.end_point
        for row in range(first, last + 1):
            line = lines[row] if row < len(lines) else b""
            before = line[:first_col] if row == first else b""
            after = line[last_col:] if row == last else b""
            if not before.strip() and not after.strip():
                found.add(row + 1)
    return found


def tokens(node: Node, source: bytes, blank_at: int = -1) -> list[bytes]:
    """Leaf texts in order, comments left out; the leaf starting at byte `blank_at` is replaced by a placeholder."""
    out: list[bytes] = []
    cursor = node.walk()
    while True:
        current = cursor.node
        if current is None:
            return out
        if current.type in COMMENT_TYPES:
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


TRANSPARENT = {"parenthesized_expression", "as_expression", "satisfies_expression", "non_null_expression", "type_assertion"}


def unwrapped(value: Node | None) -> Node | None:
    """The expression under parentheses and type assertions: `((x) => x) as Fn` is the arrow function."""
    while value is not None and value.type in TRANSPARENT:
        value = next((c for c in kids(value, True) if c.type not in ("type_annotation", "type_arguments")), None)
    return value


class _Collector:
    def __init__(self, path: str, source: bytes) -> None:
        self.path = path
        self.source = source
        self.out: list[Definition] = []

    def text(self, node: Node) -> str:
        return self.source[node.start_byte : node.end_byte].decode("utf-8", "replace")

    def add(
        self,
        extent: Node,
        name_node: Node,
        kind: str,
        container: tuple[str, ...],
        sig_nodes: list[Node | None],
        first_line: int | None = None,
        decorated: bool | None = None,
        label: str | None = None,
    ) -> None:
        """Record a definition; `first_line` pulls the extent up over attributes and doc comments that precede it."""
        start, end = _lines(extent)
        start = min(start, first_line or start)
        signature = " ".join(b.decode("utf-8", "replace") for n in sig_nodes if n is not None for b in tokens(n, self.source))
        self.out.append(
            Definition(
                path=self.path,
                kind=kind,
                name=label or self.text(name_node),
                container=container,
                start=start,
                end=end,
                name_row=name_node.start_point.row,
                name_col=name_node.start_point.column,
                signature=signature,
                body_hash=_digest(tokens(extent, self.source)),
                shape_hash=_digest(tokens(extent, self.source, name_node.start_byte)),
                decorated=(
                    decorated
                    if decorated is not None
                    else extent.type == "decorated_definition" or any(c.type == "decorator" for c in kids(extent))
                ),
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
        elif kind in TS_NAMESPACES:  # `export namespace N {}` arrives here, not through `visit`
            self.visit(node, container, in_function)
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
            value = unwrapped(declarator.child_by_field_name("value"))
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
        right = unwrapped(assignment.child_by_field_name("right"))
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
    grammar = grammar_for(path)
    if grammar == "python":
        walker: _Collector = _PythonWalker(path, source)
    elif grammar in SCRIPT_GRAMMARS:
        walker = _ScriptWalker(path, source)
    else:
        from syntax_compiled import WALKERS

        walker = WALKERS[grammar or ""](path, source)
    walker.run(tree.root_node)
    return sorted(walker.out, key=lambda d: (d.start, -d.end))


def reference_kind(root: Node, start: tuple[int, int], end: tuple[int, int], is_class: bool, grammar: str) -> str:
    """The syntactic relation of the identifier at `start`..`end` (0-based row, byte column) to what it names.

    SCIP marks no call: a callee position is `calls` (`instantiates` for a class), an argument or an assigned
    value is `passes_as_value`, a re-export clause is `reexports`, anything else `reads`; an occurrence inside an
    import statement is `imports` (R8).
    """
    return ruled_kind(root, start, end, is_class, grammar)[0]


def ruled_kind(
    root: Node, start: tuple[int, int], end: tuple[int, int], is_class: bool, grammar: str
) -> tuple[str, tuple[str, ...]]:
    """`reference_kind` and the rulings that moved it off the plain syntactic reading, in the order they ran."""
    node: Node | None = root.named_descendant_for_point_range(start, end)
    if node is None:
        return "reads", ()
    if grammar == "python":
        kind = _python_kind(node, is_class)
    elif grammar in SCRIPT_GRAMMARS:
        kind = _script_kind(node, is_class)
    else:
        from syntax_compiled import REFERENCE_KINDS

        kind = REFERENCE_KINDS[grammar](node, is_class)
        replaced = _compiled_type_use(node, kind, grammar)
        return (replaced, ("R9",)) if replaced else (kind, ())
    moved: list[str] = []
    for ruling, override in RULINGS:
        replaced = override(node, kind, grammar)
        if replaced is not None and replaced != kind:
            kind = replaced
            moved.append(ruling)
    return kind, tuple(moved)


IMPORT_STATEMENTS = {
    "python": {"import_statement", "import_from_statement", "future_import_statement"},
    "script": {"import_statement"},
}


def _within(node: Node, types: set[str]) -> bool:
    current: Node | None = node
    while current is not None:
        if current.type in types:
            return True
        current = current.parent
    return False


def _imports(node: Node, kind: str, grammar: str) -> str | None:
    """R8: a name inside an import statement is imported, never read."""
    return "imports" if _within(node, IMPORT_STATEMENTS["python" if grammar == "python" else "script"]) else None


QUALIFIED_NAMES = {"member_expression", "nested_identifier", "nested_type_identifier"}


def _type_use(node: Node, kind: str, grammar: str) -> str | None:
    """R9: an annotation, a `typeof` in a type, a type assertion or a type argument names a type, it does not read a value."""
    if kind in ("imports", "reexports"):
        return None
    if grammar == "python":
        return "type_use" if _within(node, {"type"}) else None
    if node.type == "type_identifier":
        return "type_use"
    current: Node | None = node
    while current is not None and (current is node or current.type in QUALIFIED_NAMES):
        current = current.parent
        if current is not None and current.type == "type_query":
            return "type_use"
    return None


def _property_key(node: Node, kind: str, grammar: str) -> str | None:
    """R16: the key of an object literal names the member it sets; a shorthand `{ x }` only reads `x`."""
    parent = node.parent
    if grammar == "python" or node.type != "property_identifier" or parent is None or parent.type != "pair":
        return None
    in_literal = parent.parent is not None and parent.parent.type == "object"
    return "writes" if in_literal and _same(parent.child_by_field_name("key"), node) else None


def _compiled_type_use(node: Node, kind: str, grammar: str) -> str | None:
    """R9 for Rust (a `type_identifier` read) and Kotlin (a name inside a `user_type`)."""
    if kind != "reads":
        return None
    if grammar in ("rust", "java"):
        return "type_use" if node.type == "type_identifier" else None
    return "type_use" if grammar == "kotlin" and _within(node, {"user_type"}) else None


COMMONJS_EXPORTS = {"exports", "module.exports"}


def _text(node: Node | None) -> str:
    return "" if node is None else node.text.decode("utf-8", "replace")


def _commonjs_target(left: Node | None) -> bool:
    """`exports.x`, `module.exports.x` or `module.exports`: the left side of an assignment that publishes."""
    if left is None or left.type != "member_expression":
        return False
    holder, prop = left.child_by_field_name("object"), left.child_by_field_name("property")
    return _text(holder) in COMMONJS_EXPORTS or (_text(holder) == "module" and _text(prop) == "exports")


def _export(node: Node, kind: str, grammar: str) -> str | None:
    """R19: an export statement naming a definition of its own module is that module's export of it, not a read."""
    parent = node.parent
    if grammar == "python" or kind not in ("reads", "passes_as_value") or parent is None:
        return None
    if parent.type == "export_specifier" and _same(parent.child_by_field_name("name"), node):
        return "reexports"
    if parent.type == "export_statement" and _same(parent.child_by_field_name("value"), node):
        return "reexports"
    if _same(parent.child_by_field_name("property"), node) and parent.type == "member_expression":
        owner = parent.parent  # `exports.x = x`: the name on the left is the export's own
        if owner is not None and owner.type == "assignment_expression" and _same(owner.child_by_field_name("left"), parent):
            return "reexports" if _commonjs_target(parent) else None
    if parent.type == "assignment_expression" and _same(parent.child_by_field_name("right"), node):
        return "reexports" if _commonjs_target(parent.child_by_field_name("left")) else None
    literal = parent.parent if parent.type == "pair" and _same(parent.child_by_field_name("value"), node) else None
    literal = parent if node.type == "shorthand_property_identifier" else literal
    owner = None if literal is None or literal.type != "object" else literal.parent
    if owner is not None and owner.type == "assignment_expression" and _same(owner.child_by_field_name("right"), literal):
        return "reexports" if _text(owner.child_by_field_name("left")) == "module.exports" else None
    return None


RULINGS = (("R8", _imports), ("R9", _type_use), ("R16", _property_key), ("R19", _export))


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
