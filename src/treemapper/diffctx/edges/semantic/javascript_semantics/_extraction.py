from __future__ import annotations

import logging
import re

import tree_sitter_typescript
from tree_sitter import Language, Node, Parser, Tree

from ..semantic_types import EMPTY_JS_SEMANTIC_INFO, JsSemanticInfo
from ._patterns import (
    _BUILTIN_GLOBALS,
    _CALL_EXPR_RE,
    _EXTENDS_TYPE_RE,
    _GENERIC_TYPE_INNER_RE,
    _GENERIC_TYPE_RE,
    _IDENT_RE,
    _IMPLEMENTS_TYPE_RE,
    _JS_KEYWORDS,
    _MEMBER_CALL_RE,
    _NEW_EXPR_RE,
    _OPTIONAL_CHAIN_RE,
    _RETURN_TYPE_RE,
    _TYPE_ANNOTATION_RE,
    _UTILITY_TYPES,
)

logger = logging.getLogger(__name__)

JsFragmentInfo = JsSemanticInfo

_EMPTY_INFO = EMPTY_JS_SEMANTIC_INFO

_TS_LANG = Language(tree_sitter_typescript.language_typescript())

_DECLARATION_TYPES_WITH_IDENTIFIER = frozenset(
    {
        "function_declaration",
        "generator_function_declaration",
    }
)

_DECLARATION_TYPES_WITH_TYPE_IDENTIFIER = frozenset(
    {
        "class_declaration",
        "type_alias_declaration",
        "interface_declaration",
    }
)


def _get_parser() -> Parser:
    return Parser(_TS_LANG)


def _parse_tree(code: str) -> Tree | None:
    try:
        return _get_parser().parse(code.encode("utf-8"))
    except Exception:
        logger.debug("tree-sitter failed to parse JS/TS content")
        return None


def _find_child_by_type(node: Node, child_type: str) -> Node | None:
    for child in node.children:
        if child.type == child_type:
            return child
    return None


def _find_children_by_type(node: Node, child_type: str) -> list[Node]:
    return [child for child in node.children if child.type == child_type]


def _text(node: Node) -> str:
    return node.text.decode("utf-8") if node.text else ""


def _collect_import_from_statement(node: Node) -> tuple[set[str], set[str]]:
    sources: set[str] = set()
    names: set[str] = set()

    for child in node.children:
        if child.type == "string":
            frag = _find_child_by_type(child, "string_fragment")
            if frag:
                sources.add(_text(frag))
        elif child.type == "import_clause":
            _collect_import_clause_names(child, names)

    return sources, names


def _collect_import_clause_names(clause: Node, names: set[str]) -> None:
    for child in clause.children:
        if child.type == "identifier":
            names.add(_text(child))
        elif child.type == "namespace_import":
            ident = _find_child_by_type(child, "identifier")
            if ident:
                names.add(_text(ident))
        elif child.type == "named_imports":
            for spec in child.children:
                if spec.type == "import_specifier":
                    alias = _find_child_by_type(spec, "identifier")
                    if alias:
                        names.add(_text(alias))


def _collect_require_names(node: Node, sources: set[str], names: set[str]) -> None:
    if node.type != "lexical_declaration" and node.type != "variable_declaration":
        return
    for declarator in _find_children_by_type(node, "variable_declarator"):
        value = _find_child_by_type(declarator, "call_expression")
        if value is None:
            continue
        func_node = _find_child_by_type(value, "identifier")
        if func_node is None or _text(func_node) != "require":
            continue
        args = _find_child_by_type(value, "arguments")
        if args is None:
            continue
        for arg in args.children:
            if arg.type == "string":
                frag = _find_child_by_type(arg, "string_fragment")
                if frag:
                    sources.add(_text(frag))

        name_node = _find_child_by_type(declarator, "identifier")
        if name_node:
            names.add(_text(name_node))
        pattern = _find_child_by_type(declarator, "object_pattern")
        if pattern:
            _collect_destructured_names(pattern, names)


def _collect_destructured_names(pattern: Node, names: set[str]) -> None:
    for child in pattern.children:
        if child.type == "shorthand_property_identifier_pattern":
            names.add(_text(child))
        elif child.type == "pair_pattern":
            value = child.child_by_field_name("value")
            if value and value.type == "identifier":
                names.add(_text(value))


def _collect_reexport_sources(node: Node, sources: set[str]) -> None:
    has_from = any(child.type == "from" or _text(child) == "from" for child in node.children)
    if not has_from:
        return
    for child in node.children:
        if child.type == "string":
            frag = _find_child_by_type(child, "string_fragment")
            if frag:
                sources.add(_text(frag))


def _extract_imports_full(code: str) -> tuple[frozenset[str], frozenset[str]]:
    tree = _parse_tree(code)
    if tree is None:
        return frozenset(), frozenset()

    import_sources: set[str] = set()
    imported_names: set[str] = set()

    for node in tree.root_node.children:
        if node.type == "import_statement":
            src, nms = _collect_import_from_statement(node)
            import_sources.update(src)
            imported_names.update(nms)
        elif node.type == "export_statement":
            _collect_reexport_sources(node, import_sources)
        elif node.type in ("lexical_declaration", "variable_declaration"):
            _collect_require_names(node, import_sources, imported_names)

    return frozenset(import_sources), frozenset(imported_names)


def extract_import_sources(code: str) -> frozenset[str]:
    sources, _ = _extract_imports_full(code)
    return sources


def _collect_default_export_names(node: Node) -> set[str]:
    names: set[str] = {"default"}
    for child in node.children:
        if child.type in _DECLARATION_TYPES_WITH_IDENTIFIER:
            ident = _find_child_by_type(child, "identifier")
            if ident:
                names.add(_text(ident))
        elif child.type in _DECLARATION_TYPES_WITH_TYPE_IDENTIFIER:
            ident = _find_child_by_type(child, "type_identifier")
            if ident:
                names.add(_text(ident))
        elif child.type == "identifier":
            text = _text(child)
            if text != "default":
                names.add(text)
    return names


def _collect_export_clause_names(export_clause: Node) -> set[str]:
    names: set[str] = set()
    for spec in export_clause.children:
        if spec.type == "export_specifier":
            alias = spec.child_by_field_name("alias")
            name_node = spec.child_by_field_name("name")
            if alias:
                names.add(_text(alias))
                if name_node:
                    names.add(_text(name_node))
            elif name_node:
                names.add(_text(name_node))
    return names


_EXPORTED_DECLARATION_TYPES = frozenset(
    {
        "function_declaration",
        "generator_function_declaration",
        "class_declaration",
        "lexical_declaration",
        "variable_declaration",
        "type_alias_declaration",
        "interface_declaration",
        "abstract_class_declaration",
        "enum_declaration",
    }
)


def _collect_export_names(node: Node) -> set[str]:
    has_default = any(_text(child) == "default" for child in node.children)
    if has_default:
        return _collect_default_export_names(node)

    export_clause = _find_child_by_type(node, "export_clause")
    if export_clause:
        return _collect_export_clause_names(export_clause)

    for child in node.children:
        if child.type in _EXPORTED_DECLARATION_TYPES:
            return _names_from_declaration(child)

    return set()


def _names_from_declaration(decl: Node) -> set[str]:
    names: set[str] = set()
    if decl.type in _DECLARATION_TYPES_WITH_IDENTIFIER:
        ident = _find_child_by_type(decl, "identifier")
        if ident:
            names.add(_text(ident))
    elif decl.type in _DECLARATION_TYPES_WITH_TYPE_IDENTIFIER:
        ident = _find_child_by_type(decl, "type_identifier")
        if ident:
            names.add(_text(ident))
    elif decl.type in ("abstract_class_declaration", "enum_declaration"):
        ident = _find_child_by_type(decl, "type_identifier") or _find_child_by_type(decl, "identifier")
        if ident:
            names.add(_text(ident))
    elif decl.type in ("lexical_declaration", "variable_declaration"):
        for vd in _find_children_by_type(decl, "variable_declarator"):
            ident = _find_child_by_type(vd, "identifier")
            if ident:
                names.add(_text(ident))
    return names


def _extract_exports(code: str) -> frozenset[str]:
    tree = _parse_tree(code)
    if tree is None:
        return frozenset()

    exports: set[str] = set()
    for node in tree.root_node.children:
        if node.type == "export_statement":
            exports.update(_collect_export_names(node))
    return frozenset(exports)


def _collect_define_names(node: Node) -> set[str]:
    names: set[str] = set()
    if node.type in _DECLARATION_TYPES_WITH_IDENTIFIER:
        ident = _find_child_by_type(node, "identifier")
        if ident:
            names.add(_text(ident))
    elif node.type in _DECLARATION_TYPES_WITH_TYPE_IDENTIFIER:
        ident = _find_child_by_type(node, "type_identifier")
        if ident:
            names.add(_text(ident))
    elif node.type in ("abstract_class_declaration", "enum_declaration"):
        ident = _find_child_by_type(node, "type_identifier") or _find_child_by_type(node, "identifier")
        if ident:
            names.add(_text(ident))
    elif node.type in ("lexical_declaration", "variable_declaration"):
        for vd in _find_children_by_type(node, "variable_declarator"):
            ident = _find_child_by_type(vd, "identifier")
            if ident:
                names.add(_text(ident))
    elif node.type == "export_statement":
        for child in node.children:
            names.update(_collect_define_names(child))
    return names


def _extract_defines(code: str) -> frozenset[str]:
    tree = _parse_tree(code)
    if tree is None:
        return frozenset()

    defines: set[str] = set()
    for node in tree.root_node.children:
        defines.update(_collect_define_names(node))
    return frozenset(defines)


def _extract_calls(code: str) -> frozenset[str]:
    calls: set[str] = set()

    for match in _CALL_EXPR_RE.finditer(code):
        name = match.group(1)
        if name not in _JS_KEYWORDS and name not in _BUILTIN_GLOBALS:
            calls.add(name)

    for match in _MEMBER_CALL_RE.finditer(code):
        calls.add(match.group(1))

    for match in _NEW_EXPR_RE.finditer(code):
        name = match.group(1)
        if name not in _BUILTIN_GLOBALS:
            calls.add(name)

    for match in _OPTIONAL_CHAIN_RE.finditer(code):
        calls.add(match.group(1))

    return frozenset(calls)


def _is_valid_type_ref(type_name: str, exclude_utility: bool = True) -> bool:
    if not type_name:
        return False
    if type_name in _BUILTIN_GLOBALS:
        return False
    if exclude_utility and type_name in _UTILITY_TYPES:
        return False
    return True


def _add_type_from_pattern(code: str, pattern: re.Pattern[str], refs: set[str], exclude_utility: bool = True) -> None:
    for match in pattern.finditer(code):
        type_name = match.group(1)
        if _is_valid_type_ref(type_name, exclude_utility):
            refs.add(type_name)


def _add_generic_types(code: str, refs: set[str]) -> None:
    for match in _GENERIC_TYPE_RE.finditer(code):
        for inner in _GENERIC_TYPE_INNER_RE.finditer(match.group(0)):
            name = inner.group(0)
            if name not in _UTILITY_TYPES:
                refs.add(name)


def _add_implements_types(code: str, refs: set[str]) -> None:
    for match in _IMPLEMENTS_TYPE_RE.finditer(code):
        for type_name in match.group(1).split(","):
            type_name = type_name.strip()
            if _is_valid_type_ref(type_name, exclude_utility=False):
                refs.add(type_name)


def _extract_type_refs(code: str) -> frozenset[str]:
    type_refs: set[str] = set()

    _add_type_from_pattern(code, _TYPE_ANNOTATION_RE, type_refs)
    _add_generic_types(code, type_refs)
    _add_type_from_pattern(code, _EXTENDS_TYPE_RE, type_refs, exclude_utility=False)
    _add_implements_types(code, type_refs)
    _add_type_from_pattern(code, _RETURN_TYPE_RE, type_refs)

    return frozenset(type_refs)


def _extract_references(code: str, defines: frozenset[str]) -> frozenset[str]:
    refs: set[str] = set()

    for match in _IDENT_RE.finditer(code):
        ident = match.group(1)
        if ident not in _JS_KEYWORDS and ident not in _BUILTIN_GLOBALS and ident not in defines and len(ident) >= 2:
            refs.add(ident)

    return frozenset(refs)


def analyze_javascript_fragment(code: str) -> JsFragmentInfo:
    if not code.strip():
        return _EMPTY_INFO

    import_sources, _ = _extract_imports_full(code)
    exports = _extract_exports(code)
    defines = _extract_defines(code)
    calls = _extract_calls(code)
    type_refs = _extract_type_refs(code)
    references = _extract_references(code, defines)

    return JsFragmentInfo(
        defines=defines | exports,
        references=references,
        calls=calls,
        type_refs=type_refs,
        imports=import_sources,
        exports=exports,
    )
