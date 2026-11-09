"""The oracle's own reading of what a commit did to one definition, from its base and head syntax trees.

It is independent of the engine: nothing here reads a diffctx answer. The folding table of Revision 12.1 is keyed
by what this module says (`Delta`), and `folding.py` applies the table to the SCIP relations that reach the target.
"""

from __future__ import annotations

from dataclasses import dataclass

from tree_sitter import Node

import syntax
from changed import ChangedDefinition
from model import Definition, grammar_for
from truth import Snapshot

CONSTRUCTORS = {"__init__", "constructor"}
CALLABLE_KINDS = {"function", "method"}
CLASS_KINDS = {"class", "struct"}
TYPE_KINDS = {"interface", "enum", "type", "trait", "union", "variant"}
VARIABLE_KINDS = {"variable", "property"}
FUNCTION_NODES = {
    "function_definition",
    "function_declaration",
    "generator_function_declaration",
    "function_signature",
    "method_definition",
    "abstract_method_signature",
    "method_signature",
    "function_item",
    "function_signature_item",
    "arrow_function",
    "function_expression",
    "function",
}
RETURN_FIELD = "return_type"
SEPARATORS = {"keyword_separator", "positional_separator"}


@dataclass(frozen=True)
class Delta:
    """`change`: added | removed | renamed | moved | unknown | visibility | doc_layout | body | signature_compatible |
    signature | decorators | class_body | declaration | value | other. `shape`: callable | class | type | variable | other."""

    change: str
    shape: str
    decorated: bool = False
    typed: bool = False  # a callable's parameters and return type are declared; a variable's type is
    constructor: bool = False
    parameters: tuple[str, ...] = ()  # the parameters a compatible change added


def shape_of(definition: Definition) -> str:
    if definition.kind in CALLABLE_KINDS:
        return "callable"
    if definition.kind in CLASS_KINDS:
        return "class"
    if definition.kind in TYPE_KINDS:
        return "type"
    if definition.kind in VARIABLE_KINDS:
        return "variable"
    return "other"


def _named(snapshot: Snapshot, definition: Definition) -> Node | None:
    tree = snapshot.parsed(definition.path)
    if tree is None:
        return None
    start = (definition.name_row, definition.name_col)
    return tree.root_node.named_descendant_for_point_range(start, (start[0], start[1] + len(definition.name.encode())))


def _declaration(name: Node | None) -> Node | None:
    """The node that declares what `name` names: a function, a class, a declarator's value or an assigned function."""
    node = name
    while node is not None and node.parent is not None:
        parent = node.parent
        if parent.type == "variable_declarator" and parent.child_by_field_name("name") == node:
            return syntax.unwrapped(parent.child_by_field_name("value")) or parent
        if parent.type == "member_expression" and parent.parent is not None and parent.parent.type == "assignment_expression":
            return syntax.unwrapped(parent.parent.child_by_field_name("right"))
        if parent.type in FUNCTION_NODES or parent.type.endswith(("_definition", "_declaration", "_item", "_header")):
            return parent
        node = parent
    return node


def _tokens(node: Node | None, source: bytes) -> list[str]:
    return [] if node is None else [t.decode("utf-8", "replace") for t in syntax.tokens(node, source)]


def _decorators(declaration: Node | None, source: bytes) -> list[str]:
    """Texts of the decorators, annotations or attributes in front of a declaration."""
    if declaration is None:
        return []
    found: list[Node] = []
    parent = declaration.parent
    if parent is not None and parent.type in ("decorated_definition", "export_statement"):
        found += [c for c in syntax.kids(parent, True) if c.type == "decorator"]
    found += [c for c in syntax.kids(declaration, True) if c.type == "decorator"]
    modifiers = next((c for c in syntax.kids(declaration, True) if c.type == "modifiers"), None)
    if modifiers is not None:
        found += [c for c in syntax.kids(modifiers, True) if c.type == "annotation"]
    sibling = declaration.prev_sibling
    while sibling is not None and sibling.type in ("attribute_item", "line_comment", "block_comment"):
        if sibling.type == "attribute_item":
            found.append(sibling)
        sibling = sibling.prev_sibling
    return [" ".join(_tokens(node, source)) for node in found]


def _parameters(declaration: Node | None, grammar: str, source: bytes) -> list[tuple[str, bool]]:
    """(tokens, optional) per parameter; optional = has a default, is a rest parameter or a separator."""
    if declaration is None:
        return []
    holder = declaration.child_by_field_name("parameters")
    if holder is None:
        holder = next(
            (c for c in syntax.kids(declaration, True) if c.type in ("formal_parameters", "function_value_parameters")), None
        )
    if holder is None:
        return []
    found: list[tuple[str, bool]] = []
    for child in syntax.kids(holder, True):
        text = " ".join(_tokens(child, source))
        if grammar == "kotlin" and child.type != "parameter" and found:
            found[-1] = (f"{found[-1][0]} = {text}", True)  # a default sits beside its parameter
        elif grammar == "python":
            found.append(
                (
                    text,
                    child.type
                    in (
                        "default_parameter",
                        "typed_default_parameter",
                        "list_splat_pattern",
                        "dictionary_splat_pattern",
                        *SEPARATORS,
                    ),
                )
            )
        elif grammar in syntax.SCRIPT_GRAMMARS:
            rest = any(c.type == "rest_pattern" for c in syntax.kids(child))
            found.append((text, child.type == "optional_parameter" or child.child_by_field_name("value") is not None or rest))
        else:
            found.append((text, False))
    return found


def _return_type(declaration: Node | None, grammar: str, source: bytes) -> str:
    if declaration is None:
        return ""
    stated = declaration.child_by_field_name(RETURN_FIELD)
    if stated is None and grammar == "kotlin":
        kids = syntax.kids(declaration, True)
        after = next((i for i, c in enumerate(kids) if c.type == "function_value_parameters"), None)
        if after is not None and after + 1 < len(kids) and kids[after + 1].type not in ("function_body", "type_constraints"):
            stated = kids[after + 1]
    return " ".join(_tokens(stated, source))


def _is_block_body(declaration: Node | None) -> bool:
    body = None if declaration is None else declaration.child_by_field_name("body")
    if body is None and declaration is not None:
        body = next((c for c in syntax.kids(declaration, True) if c.type == "function_body"), None)
    return body is not None and (body.type in ("block", "statement_block") or any(c.type == "block" for c in syntax.kids(body)))


def _typed_callable(declaration: Node | None, grammar: str, source: bytes) -> bool:
    """The parameters and the return type are written down (Rev 12.1: Python annotated, TS explicit return type,
    Rust always, Kotlin a block body with a declared return type)."""
    if declaration is None:
        return False
    if grammar == "rust":
        return True
    returns = bool(_return_type(declaration, grammar, source))
    if grammar == "python":
        holder = declaration.child_by_field_name("parameters")
        params = syntax.kids(holder, True) if holder is not None else []
        needed = [c for c in params if c.type not in SEPARATORS and _tokens(c, source) not in (["self"], ["cls"])]
        return returns and all(c.type in ("typed_parameter", "typed_default_parameter") for c in needed)
    if grammar == "kotlin":
        return returns and _is_block_body(declaration)
    holder = declaration.child_by_field_name("parameters")
    typed_params = (
        all(c.child_by_field_name("type") is not None for c in syntax.kids(holder, True)) if holder is not None else True
    )
    return returns and typed_params


def _declared_variable_type(name: Node | None, grammar: str, source: bytes) -> str:
    """The text of a variable's type annotation, empty when it is inferred."""
    parent = None if name is None else name.parent
    while parent is not None and parent.type not in (
        "assignment",
        "variable_declarator",
        "const_item",
        "static_item",
        "property_declaration",
        "public_field_definition",
        "field_definition",
    ):
        parent = parent.parent
    if parent is None:
        return ""
    stated = parent.child_by_field_name("type")
    if stated is None and grammar == "kotlin":
        declaration = next((c for c in syntax.kids(parent, True) if c.type == "variable_declaration"), None)
        stated = (
            next((c for c in syntax.kids(declaration, True) if c.type != "identifier"), None) if declaration is not None else None
        )
    return " ".join(_tokens(stated, source))


def _visibility(declaration: Node | None, grammar: str, source: bytes) -> int:
    """3 public, 2 protected or crate-wide, 1 private to its module or class."""
    if declaration is None:
        return 3
    if grammar == "python":
        return 3
    if grammar == "rust":
        modifier = next((c for c in syntax.kids(declaration, True) if c.type == "visibility_modifier"), None)
        if modifier is None:
            return 1
        return 3 if _tokens(modifier, source) == ["pub"] else 2
    if grammar == "kotlin":
        modifiers = next((c for c in syntax.kids(declaration, True) if c.type == "modifiers"), None)
        words = _tokens(modifiers, source)
        return 1 if "private" in words else 2 if ("protected" in words or "internal" in words) else 3
    accessibility = next((c for c in syntax.kids(declaration, True) if c.type == "accessibility_modifier"), None)
    if accessibility is not None:
        word = _tokens(accessibility, source)
        return 1 if word == ["private"] else 2 if word == ["protected"] else 3
    parent = declaration.parent
    exported = parent is not None and parent.type == "export_statement"
    member = parent is not None and parent.type in ("class_body", "object_type", "interface_body", "enum_body")
    return 3 if exported or member else 1


def _without_docstring(declaration: Node | None, grammar: str, source: bytes) -> list[str]:
    """The tokens of a declaration with a leading Python docstring left out (comments are never tokens)."""
    tokens = _tokens(declaration, source)
    if declaration is None or grammar != "python":
        return tokens
    body = declaration.child_by_field_name("body")
    first = next(iter(syntax.kids(body, True)), None) if body is not None else None
    doc = (
        first
        if first is not None
        and first.type == "expression_statement"
        and any(c.type == "string" for c in syntax.kids(first, True))
        else None
    )
    if doc is None:
        return tokens
    inner = _tokens(doc, source)
    for i in range(len(tokens) - len(inner) + 1):
        if tokens[i : i + len(inner)] == inner:
            return tokens[:i] + tokens[i + len(inner) :]
    return tokens


def _ambiguous(snapshot: Snapshot, definition: Definition) -> bool:
    """Several definitions of one file share the container and the name: the pairing across the commit is a guess."""
    twins = [
        d for d in snapshot.definitions(definition.path) if d.container == definition.container and d.name == definition.name
    ]
    return len(twins) > 1


def classify(item: ChangedDefinition, base: Snapshot, head: Snapshot) -> Delta:
    target = item.target
    shape = shape_of(target)
    if item.status in ("added", "removed", "renamed", "moved"):
        return Delta(item.status, shape)
    before, after = item.base, item.head
    assert before is not None and after is not None
    grammar = grammar_for(after.path) or ""
    constructor = after.name in CONSTRUCTORS
    if _ambiguous(head, after) or _ambiguous(base, before):
        return Delta("unknown", shape, constructor=constructor)
    old, new = _declaration(_named(base, before)), _declaration(_named(head, after))
    old_source, new_source = base.source(before.path), head.source(after.path)
    decorated = after.decorated
    if _visibility(new, grammar, new_source) < _visibility(old, grammar, old_source):
        return Delta("visibility", shape, decorated, constructor=constructor)
    if shape == "type":
        return Delta("declaration", shape, decorated)
    if _decorators(old, old_source) != _decorators(new, new_source):
        return Delta("decorators", shape, decorated, constructor=constructor)
    if shape == "variable":
        old_type = _declared_variable_type(_named(base, before), grammar, old_source)
        new_type = _declared_variable_type(_named(head, after), grammar, new_source)
        return Delta("value" if old_type == new_type else "other", shape, decorated, typed=bool(new_type))
    if shape == "class":
        same = before.signature == after.signature
        return Delta("class_body" if same else "other", shape, decorated)
    if shape != "callable":
        return Delta("other", shape, decorated)
    typed = _typed_callable(new, grammar, new_source)
    if before.signature == after.signature:
        if _without_docstring(old, grammar, old_source) == _without_docstring(new, grammar, new_source):
            return Delta("doc_layout", shape, decorated, typed, constructor)
        return Delta("body", shape, decorated, typed, constructor)
    added = _added_optional(old, new, grammar, old_source, new_source)
    if added:
        return Delta("signature_compatible", shape, decorated, typed, constructor, added)
    return Delta("signature", shape, decorated, typed, constructor)


def _added_optional(old: Node | None, new: Node | None, grammar: str, old_source: bytes, new_source: bytes) -> tuple[str, ...]:
    """The parameters appended after all the old ones, each with a default (never in Rust); empty when the change is
    anything else, the return type included."""
    if grammar == "rust" or _return_type(old, grammar, old_source) != _return_type(new, grammar, new_source):
        return ()
    before, after = _parameters(old, grammar, old_source), _parameters(new, grammar, new_source)
    if len(after) <= len(before) or [t for t, _ in after[: len(before)]] != [t for t, _ in before]:
        return ()
    extra = after[len(before) :]
    return tuple(t for t, _ in extra) if all(optional for _, optional in extra) else ()
