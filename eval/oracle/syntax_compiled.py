"""Definitions and reference contexts for Rust, Go and Kotlin, on the walker base of syntax.py."""

from __future__ import annotations

from collections.abc import Callable

from tree_sitter import Node

from syntax import COMMENT_TYPES, _Collector, kids


def _same(a: Node | None, b: Node) -> bool:
    return a is not None and a == b


def leading_line(node: Node, extra: tuple[str, ...] = ()) -> int:
    """First line of the attributes and doc comments that touch `node` from above, or its own first line."""
    first = node.start_point.row + 1
    sibling = node.prev_sibling
    while sibling is not None and (sibling.type in COMMENT_TYPES or sibling.type in extra):
        if sibling.end_point.row + 2 < first:
            break
        first = sibling.start_point.row + 1
        sibling = sibling.prev_sibling
    return first


def first_of_type(node: Node, kind: str) -> Node | None:
    """The first descendant of a type, depth first."""
    stack = [node]
    while stack:
        current = stack.pop()
        if current.type == kind:
            return current
        stack.extend(reversed(kids(current)))
    return None


def named_fields(node: Node, field: str) -> list[Node]:
    """Every child carrying a field name; `child_by_field_name` returns only the first."""
    found: list[Node] = []
    cursor = node.walk()
    if cursor.goto_first_child():
        while True:
            if cursor.field_name == field and cursor.node is not None:
                found.append(cursor.node)
            if not cursor.goto_next_sibling():
                break
    return found


def fields(node: Node, *names: str) -> list[Node | None]:
    return [node.child_by_field_name(name) for name in names]


RUST_ITEMS = {
    "function_item": "function",
    "function_signature_item": "function",
    "struct_item": "struct",
    "enum_item": "enum",
    "union_item": "union",
    "trait_item": "trait",
    "type_item": "type",
    "associated_type": "type",
    "const_item": "variable",
    "static_item": "variable",
    "macro_definition": "macro",
    "mod_item": "module",
    "field_declaration": "property",
    "enum_variant": "variant",
}
RUST_SIGNATURE = ("type_parameters", "parameters", "return_type", "type")


class RustWalker(_Collector):
    def run(self, root: Node) -> None:
        for child in kids(root, True):
            self.visit(child, ())

    def visit(self, node: Node, container: tuple[str, ...]) -> None:
        if node.type == "impl_item":
            body = node.child_by_field_name("body")
            inner = (*container, self.impl_type(node))
            for child in kids(body, True) if body is not None else ():
                self.visit(child, inner)
        elif node.type in RUST_ITEMS:
            self.item(node, container)
        else:
            for child in kids(node, True):
                self.visit(child, container)

    def impl_type(self, node: Node) -> str:
        target = node.child_by_field_name("type")
        while target is not None and target.type in ("generic_type", "scoped_type_identifier", "reference_type"):
            target = target.child_by_field_name("name" if target.type == "scoped_type_identifier" else "type")
        return self.text(target) if target is not None else ""

    def item(self, node: Node, container: tuple[str, ...]) -> None:
        name = node.child_by_field_name("name")
        if name is None:
            return
        before = node.prev_sibling
        self.add(
            node,
            name,
            RUST_ITEMS[node.type],
            container,
            fields(node, *RUST_SIGNATURE),
            first_line=leading_line(node, ("attribute_item",)),
            decorated=before is not None and before.type == "attribute_item",
        )
        body = node.child_by_field_name("body")
        inner = (*container, self.text(name))
        for child in kids(body, True) if body is not None else ():
            self.visit(child, inner)


class GoWalker(_Collector):
    def run(self, root: Node) -> None:
        for child in kids(root, True):
            self.visit(child)

    def visit(self, node: Node) -> None:
        if node.type == "function_declaration":
            self.callable(node, "function", ())
        elif node.type == "method_declaration":
            receiver = node.child_by_field_name("receiver")
            owner = first_of_type(receiver, "type_identifier") if receiver is not None else None
            self.callable(node, "method", (self.text(owner),) if owner is not None else ())
        elif node.type == "type_declaration":
            specs = [c for c in kids(node, True) if c.type in ("type_spec", "type_alias")]
            for spec in specs:
                self.type_spec(spec, node if len(specs) == 1 else spec)
        elif node.type in ("const_declaration", "var_declaration"):
            specs = self.specs(node)
            for spec in specs:
                extent = node if len(specs) == 1 else spec
                for name in named_fields(spec, "name"):
                    self.add(extent, name, "variable", (), [spec.child_by_field_name("type")], first_line=leading_line(extent))

    def specs(self, node: Node) -> list[Node]:
        found: list[Node] = []
        for child in kids(node, True):
            if child.type in ("const_spec", "var_spec"):
                found.append(child)
            elif child.type == "var_spec_list":
                found += [c for c in kids(child, True) if c.type == "var_spec"]
        return found

    def callable(self, node: Node, kind: str, container: tuple[str, ...]) -> None:
        name = node.child_by_field_name("name")
        if name is not None:
            sig = fields(node, "type_parameters", "parameters", "result")
            self.add(node, name, kind, container, sig, first_line=leading_line(node))

    def type_spec(self, spec: Node, extent: Node) -> None:
        name = spec.child_by_field_name("name")
        if name is None:
            return
        shape = spec.child_by_field_name("type")
        kind = {"struct_type": "struct", "interface_type": "interface"}.get(shape.type if shape is not None else "", "type")
        self.add(extent, name, kind, (), [spec.child_by_field_name("type_parameters")], first_line=leading_line(extent))
        owner = (self.text(name),)
        if shape is not None and shape.type == "struct_type":
            for declaration in (c for lst in kids(shape, True) if lst.type == "field_declaration_list" for c in kids(lst, True)):
                for field in named_fields(declaration, "name"):
                    self.add(declaration, field, "property", owner, [declaration.child_by_field_name("type")])
        elif shape is not None and shape.type == "interface_type":
            for element in (c for c in kids(shape, True) if c.type == "method_elem"):
                label = element.child_by_field_name("name")
                if label is not None:
                    self.add(element, label, "method", owner, fields(element, "parameters", "result"))


KOTLIN_CLASSES = {"class_declaration", "object_declaration"}
KOTLIN_BODIES = {"class_body", "enum_class_body"}
KOTLIN_NOT_HEADER = {"modifiers", "function_body", "class_body", "enum_class_body", "annotation"}


class KotlinWalker(_Collector):
    def run(self, root: Node) -> None:
        for child in kids(root, True):
            self.visit(child, (), False)

    def visit(self, node: Node, container: tuple[str, ...], in_function: bool) -> None:
        kind = node.type
        if kind in KOTLIN_CLASSES or kind == "companion_object":
            self.type_like(node, container)
        elif kind == "function_declaration":
            self.function(node, container, in_function)
        elif kind == "property_declaration":
            if not in_function:
                self.property(node, container)
        elif kind == "secondary_constructor":
            keyword = next((c for c in kids(node) if c.type == "constructor"), None)
            if keyword is not None:
                self.add(node, keyword, "method", container, self.header(node, keyword), first_line=leading_line(node))
        elif kind == "enum_entry":
            entry = next((c for c in kids(node, True) if c.type == "identifier"), None)
            if entry is not None:
                self.add(node, entry, "variant", container, [])
        elif kind == "type_alias":
            name = node.child_by_field_name("type")
            if name is not None:
                self.add(node, name, "type", container, [], first_line=leading_line(node))
        else:
            for child in kids(node, True):
                self.visit(child, container, in_function)

    def header(self, node: Node, name: Node) -> list[Node | None]:
        return [c for c in kids(node, True) if c.type not in KOTLIN_NOT_HEADER and c != name and c.type != "identifier"]

    def type_like(self, node: Node, container: tuple[str, ...]) -> None:
        name = node.child_by_field_name("name") or next((c for c in kids(node, True) if c.type == "identifier"), None)
        label = self.text(name) if name is not None else "Companion"
        kind = "interface" if any(c.type == "interface" for c in kids(node)) else "class"
        if name is not None:
            self.add(node, name, kind, container, self.header(node, name), first_line=leading_line(node))
        inner = (*container, label)
        for child in kids(node, True):
            if child.type == "primary_constructor":
                self.constructor_properties(child, inner)
            elif child.type in KOTLIN_BODIES:
                for member in kids(child, True):
                    self.visit(member, inner, False)

    def constructor_properties(self, constructor: Node, owner: tuple[str, ...]) -> None:
        for parameters in (c for c in kids(constructor, True) if c.type == "class_parameters"):
            for parameter in (c for c in kids(parameters, True) if c.type == "class_parameter"):
                declares = any(c.type in ("val", "var") for c in kids(parameter))
                label = next((c for c in kids(parameter, True) if c.type == "identifier"), None)
                if declares and label is not None:
                    self.add(parameter, label, "property", owner, [])

    def function(self, node: Node, container: tuple[str, ...], in_function: bool) -> None:
        name = node.child_by_field_name("name")
        if name is None:
            return
        kind = "method" if container and not in_function else "function"
        self.add(node, name, kind, container, self.header(node, name), first_line=leading_line(node))
        inner = (*container, self.text(name))
        for child in kids(node, True):
            if child.type == "function_body":
                for statement in kids(child, True):
                    self.visit(statement, inner, True)

    def property(self, node: Node, container: tuple[str, ...]) -> None:
        declaration = next((c for c in kids(node, True) if c.type == "variable_declaration"), None)
        label = next((c for c in kids(declaration, True) if c.type == "identifier"), None) if declaration is not None else None
        if label is not None:
            self.add(node, label, "variable", container, [], first_line=leading_line(node))


JAVA_TYPES = {
    "class_declaration": "class",
    "record_declaration": "class",
    "interface_declaration": "interface",
    "annotation_type_declaration": "interface",
    "enum_declaration": "enum",
}
JAVA_HERITAGE = ("type_parameters", "superclass", "interfaces", "permits", "parameters")
JAVA_CONSTRUCTORS = {"constructor_declaration", "compact_constructor_declaration"}


class JavaWalker(_Collector):
    """Types, methods, constructors (named `constructor`, as the TypeScript and Kotlin ones), fields and enum constants;
    method bodies are not entered, so local and anonymous classes are no definitions."""

    def run(self, root: Node) -> None:
        for child in kids(root, True):
            self.visit(child, ())

    def visit(self, node: Node, container: tuple[str, ...]) -> None:
        kind = node.type
        if kind in JAVA_TYPES:
            self.type_like(node, container)
        elif kind == "method_declaration":
            self.member(node, container, "method", None, fields(node, "type_parameters", "type", "parameters"))
        elif kind in JAVA_CONSTRUCTORS:
            self.member(node, container, "method", "constructor", fields(node, "parameters"))
        elif kind == "field_declaration":
            for declarator in named_fields(node, "declarator"):
                name = declarator.child_by_field_name("name")
                if name is not None:
                    self.add(
                        node,
                        name,
                        "property",
                        container,
                        fields(node, "type"),
                        first_line=leading_line(node),
                        decorated=self.annotated(node),
                    )
        elif kind == "enum_constant":
            name = node.child_by_field_name("name")
            if name is not None:
                self.add(node, name, "variant", container, [], first_line=leading_line(node))
                body = node.child_by_field_name("body")
                for member in kids(body, True) if body is not None else ():
                    self.visit(member, (*container, self.text(name)))  # an enum constant's body is its own class
        else:
            for child in kids(node, True):
                self.visit(child, container)

    def annotated(self, node: Node) -> bool:
        modifiers = next((c for c in kids(node, True) if c.type == "modifiers"), None)
        return modifiers is not None and any(c.type in ("marker_annotation", "annotation") for c in kids(modifiers, True))

    def member(self, node: Node, container: tuple[str, ...], kind: str, label: str | None, signature: list[Node | None]) -> None:
        name = node.child_by_field_name("name")
        if name is not None:
            self.add(
                node, name, kind, container, signature, first_line=leading_line(node), decorated=self.annotated(node), label=label
            )

    def type_like(self, node: Node, container: tuple[str, ...]) -> None:
        name = node.child_by_field_name("name")
        if name is None:
            return
        self.add(
            node,
            name,
            JAVA_TYPES[node.type],
            container,
            fields(node, *JAVA_HERITAGE),
            first_line=leading_line(node),
            decorated=self.annotated(node),
        )
        inner = (*container, self.text(name))
        body = node.child_by_field_name("body")
        for member in kids(body, True) if body is not None else ():
            self.visit(member, inner)


WALKERS: dict[str, type[_Collector]] = {"rust": RustWalker, "go": GoWalker, "kotlin": KotlinWalker, "java": JavaWalker}


def _ancestor(node: Node, kind: str) -> bool:
    current: Node | None = node
    while current is not None:
        if current.type == kind:
            return True
        current = current.parent
    return False


def _rust_kind(node: Node, is_class: bool) -> str:
    climb = {
        "scoped_identifier": "name",
        "field_expression": "field",
        "generic_function": "function",
        "scoped_type_identifier": "name",
    }
    while (
        node.parent is not None
        and node.parent.type in climb
        and _same(node.parent.child_by_field_name(climb[node.parent.type]), node)
    ):
        node = node.parent
    parent = node.parent
    if parent is None:
        return "reads"
    if _ancestor(parent, "use_declaration"):
        return "imports"
    if parent.type == "call_expression" and _same(parent.child_by_field_name("function"), node):
        return "instantiates" if is_class else "calls"
    if parent.type == "macro_invocation" and _same(parent.child_by_field_name("macro"), node):
        return "calls"
    if parent.type == "struct_expression" and _same(parent.child_by_field_name("name"), node):
        return "instantiates"
    if parent.type == "arguments":
        return "passes_as_value"
    value_of = {"let_declaration": "value", "assignment_expression": "right", "field_initializer": "value"}
    if parent.type in value_of and _same(parent.child_by_field_name(value_of[parent.type]), node):
        return "passes_as_value"
    return "reads"


def _go_kind(node: Node, is_class: bool) -> str:
    climb = {"selector_expression": "field", "qualified_type": "name"}
    while (
        node.parent is not None
        and node.parent.type in climb
        and _same(node.parent.child_by_field_name(climb[node.parent.type]), node)
    ):
        node = node.parent
    parent = node.parent
    if parent is None:
        return "reads"
    if _ancestor(parent, "import_spec"):
        return "imports"
    if parent.type == "call_expression" and _same(parent.child_by_field_name("function"), node):
        return "calls"
    if parent.type == "composite_literal" and _same(parent.child_by_field_name("type"), node):
        return "instantiates"
    if parent.type == "argument_list":
        return "passes_as_value"
    if parent.type == "expression_list" and parent.parent is not None:
        owner = parent.parent
        side = {"short_var_declaration": "right", "assignment_statement": "right", "var_spec": "value", "const_spec": "value"}
        if owner.type in side and _same(owner.child_by_field_name(side[owner.type]), parent):
            return "passes_as_value"
    if parent.type == "literal_element" and parent.parent is not None and parent.parent.type == "keyed_element":
        if _same(parent.parent.child_by_field_name("value"), parent):
            return "passes_as_value"
    return "reads"


def _kotlin_kind(node: Node, is_class: bool) -> str:
    while node.parent is not None and node.parent.type == "navigation_expression":
        named = kids(node.parent, True)
        if not named or named[-1] != node:
            break
        node = node.parent
    parent = node.parent
    if parent is None:
        return "reads"
    if _ancestor(parent, "import"):
        return "imports"
    named = kids(parent, True)
    if parent.type == "call_expression" and named and named[0] == node:
        return "instantiates" if is_class else "calls"
    if parent.type in ("value_argument", "callable_reference"):
        return "passes_as_value"
    if parent.type in ("property_declaration", "assignment") and named and named[-1] == node:
        return "passes_as_value"
    return "reads"


def _java_kind(node: Node, is_class: bool) -> str:
    while node.parent is not None and (
        (node.parent.type == "field_access" and _same(node.parent.child_by_field_name("field"), node))
        or (node.parent.type == "scoped_identifier" and _same(node.parent.child_by_field_name("name"), node))
        or (
            node.parent.type == "generic_type"
            and node.parent.parent is not None
            and node.parent.parent.type == "object_creation_expression"
        )
    ):
        node = node.parent
    parent = node.parent
    if parent is None:
        return "reads"
    if _ancestor(parent, "import_declaration"):
        return "imports"
    if parent.type == "method_invocation" and _same(parent.child_by_field_name("name"), node):
        return "calls"
    if parent.type == "object_creation_expression" and _same(parent.child_by_field_name("type"), node):
        return "instantiates"
    if parent.type in ("method_reference", "argument_list"):
        return "passes_as_value"
    value_of = {"variable_declarator": "value", "assignment_expression": "right"}
    if parent.type in value_of and _same(parent.child_by_field_name(value_of[parent.type]), node):
        return "passes_as_value"
    return "reads"


REFERENCE_KINDS: dict[str, Callable[[Node, bool], str]] = {
    "rust": _rust_kind,
    "go": _go_kind,
    "kotlin": _kotlin_kind,
    "java": _java_kind,
}
