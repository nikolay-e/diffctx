from __future__ import annotations

from pathlib import Path
from typing import Any

from tree_sitter import Language, Node, Parser

from ..languages import TREE_SITTER_LANGUAGES
from ..types import Fragment, FragmentId, extract_identifiers
from .base import MIN_FRAGMENT_LINES, create_code_gap_fragments, create_snippet

_SUB_FRAGMENT_THRESHOLD_LINES = 30
_SUB_FRAGMENT_TARGET_LINES = 20
_BODY_FIELD_NAMES = ("body", "block", "consequence")
_MAX_SUB_DEPTH = 3

_DEFINITION_NODE_TYPES = {
    "python": {"function_definition", "class_definition", "decorated_definition"},
    "javascript": {"function_declaration", "class_declaration", "method_definition", "arrow_function", "variable_declarator"},
    "jsx": {"function_declaration", "class_declaration", "method_definition", "arrow_function", "variable_declarator"},
    "typescript": {
        "function_declaration",
        "class_declaration",
        "method_definition",
        "arrow_function",
        "interface_declaration",
        "type_alias_declaration",
        "enum_declaration",
        "variable_declarator",
    },
    "tsx": {
        "function_declaration",
        "class_declaration",
        "method_definition",
        "arrow_function",
        "interface_declaration",
        "type_alias_declaration",
        "enum_declaration",
        "variable_declarator",
    },
    "go": {"function_declaration", "method_declaration", "type_declaration", "const_declaration", "var_declaration"},
    "rust": {
        "function_item",
        "impl_item",
        "struct_item",
        "enum_item",
        "trait_item",
        "mod_item",
        "const_item",
        "static_item",
        "macro_definition",
        "type_item",
    },
    "java": {"method_declaration", "class_declaration", "interface_declaration", "enum_declaration", "constructor_declaration"},
    "c": {"function_definition", "struct_specifier", "enum_specifier", "declaration", "type_definition"},
    "cpp": {
        "function_definition",
        "class_specifier",
        "struct_specifier",
        "enum_specifier",
        "declaration",
        "type_definition",
        "using_declaration",
        "alias_declaration",
    },
    "ruby": {"method", "class", "module", "singleton_method"},
    "c_sharp": {
        "method_declaration",
        "class_declaration",
        "interface_declaration",
        "struct_declaration",
        "enum_declaration",
        "record_declaration",
        "property_declaration",
        "constructor_declaration",
    },
}

_NODE_TYPE_KEYWORDS = [
    (("function", "method"), "function"),
    (("class",), "class"),
    (("struct",), "struct"),
    (("impl",), "impl"),
    (("trait", "interface"), "interface"),
    (("enum",), "enum"),
    (("module",), "module"),
    (("type_alias", "alias_declaration", "type_definition"), "type"),
    (("variable_declarator",), "variable"),
    (("record",), "record"),
    (("property",), "property"),
    (("declaration", "using_declaration"), "declaration"),
]

_CONTAINER_KINDS = frozenset({"class", "interface", "struct", "impl"})

_MAX_RECURSION_DEPTH = 500

_LANG_MODULES = {
    "python": "tree_sitter_python",
    "javascript": "tree_sitter_javascript",
    "jsx": "tree_sitter_javascript",
    "typescript": "tree_sitter_typescript",
    "tsx": "tree_sitter_typescript",
    "go": "tree_sitter_go",
    "rust": "tree_sitter_rust",
    "java": "tree_sitter_java",
    "c": "tree_sitter_c",
    "cpp": "tree_sitter_cpp",
    "ruby": "tree_sitter_ruby",
    "c_sharp": "tree_sitter_c_sharp",
    "csharp": "tree_sitter_c_sharp",
    "elixir": "tree_sitter_elixir",
    "lua": "tree_sitter_lua",
    "ocaml": "tree_sitter_ocaml",
    "php": "tree_sitter_php",
    "scala": "tree_sitter_scala",
    "swift": "tree_sitter_swift",
}


def _import_lang_module(lang: str) -> Any | None:
    import importlib

    module_name = _LANG_MODULES.get(lang)
    if not module_name:
        return None
    try:
        return importlib.import_module(module_name)
    except ImportError:
        return None


def get_ts_language(lang: str) -> Language | None:
    mod = _import_lang_module(lang)
    if mod is None:
        return None
    if lang == "tsx":
        return Language(mod.language_tsx())
    if lang == "typescript":
        return Language(mod.language_typescript())
    if lang == "ocaml" and hasattr(mod, "language_ocaml"):
        return Language(mod.language_ocaml())
    if lang == "php" and hasattr(mod, "language_php"):
        return Language(mod.language_php())
    if hasattr(mod, "language"):
        return Language(mod.language())
    return None


class TreeSitterStrategy:
    priority = 100

    def __init__(self) -> None:
        self._parsers: dict[str, Parser | None] = {}

    def _get_parser(self, lang: str) -> Parser:
        cached = self._parsers.get(lang)
        if cached is not None:
            return cached
        if lang in self._parsers:
            raise ValueError(f"Grammar unavailable for language: {lang}")

        ts_lang = get_ts_language(lang)
        if ts_lang is None:
            self._parsers[lang] = None
            raise ValueError(f"Grammar unavailable for language: {lang}")

        parser = Parser()
        parser.language = ts_lang
        self._parsers[lang] = parser
        return parser

    def can_handle(self, path: Path, _content: str) -> bool:
        suffix = path.suffix.lower()
        return suffix in TREE_SITTER_LANGUAGES

    def fragment(self, path: Path, content: str) -> list[Fragment]:
        suffix = path.suffix.lower()
        lang = TREE_SITTER_LANGUAGES[suffix]
        parser = self._get_parser(lang)

        code_bytes = content.encode("utf-8")
        tree = parser.parse(code_bytes)
        lines = content.splitlines()

        fragments: list[Fragment] = []
        covered: set[tuple[int, int]] = set()

        definition_types = _DEFINITION_NODE_TYPES.get(lang, set())
        self._extract_definitions(tree.root_node, code_bytes, path, lines, definition_types, fragments, covered)

        gap_frags = create_code_gap_fragments(path, lines, list(covered))
        fragments.extend(gap_frags)

        return fragments

    def _extract_definitions(
        self,
        node: Node,
        code_bytes: bytes,
        path: Path,
        lines: list[str],
        definition_types: set[str],
        fragments: list[Fragment],
        covered: set[tuple[int, int]],
        added_ends: set[tuple[str, int]] | None = None,
        depth: int = 0,
    ) -> None:
        if added_ends is None:
            added_ends = set()
        if depth > _MAX_RECURSION_DEPTH:
            return

        if node.type in definition_types:
            self._handle_definition_node(node, code_bytes, path, lines, definition_types, fragments, covered, added_ends, depth)
        else:
            self._recurse_children(node, code_bytes, path, lines, definition_types, fragments, covered, added_ends, depth)

    def _handle_definition_node(
        self,
        node: Node,
        code_bytes: bytes,
        path: Path,
        lines: list[str],
        definition_types: set[str],
        fragments: list[Fragment],
        covered: set[tuple[int, int]],
        added_ends: set[tuple[str, int]],
        depth: int,
    ) -> None:
        start = node.start_point[0] + 1
        end = node.end_point[0] + 1
        kind = self._node_type_to_kind(node.type, node)

        if (kind, end) in added_ends:
            self._recurse_children(node, code_bytes, path, lines, definition_types, fragments, covered, added_ends, depth)
            return

        sym_name = self._extract_symbol_name(node)

        ancestor = node.parent
        if ancestor is not None and ancestor.type not in ("export_statement", "decorated_definition"):
            if ancestor.parent is not None and ancestor.parent.type in ("export_statement", "decorated_definition"):
                ancestor = ancestor.parent
        if ancestor is not None and ancestor.type in ("export_statement", "decorated_definition"):
            ancestor_start = ancestor.start_point[0] + 1
            if ancestor_start < start:
                start = ancestor_start

        if kind in _CONTAINER_KINDS and self._try_container_split(
            node, code_bytes, path, lines, definition_types, fragments, covered, added_ends, depth, start, end, kind, sym_name
        ):
            return

        self._add_leaf_definition(path, lines, start, end, kind, sym_name, fragments, covered, added_ends)
        if end - start + 1 > _SUB_FRAGMENT_THRESHOLD_LINES:
            self._create_sub_fragments(node, path, lines, sym_name, fragments, covered)
        if node.type == "variable_declarator" and self._has_function_child(node):
            return
        self._recurse_children(node, code_bytes, path, lines, definition_types, fragments, covered, added_ends, depth)

    def _try_container_split(
        self,
        node: Node,
        code_bytes: bytes,
        path: Path,
        lines: list[str],
        definition_types: set[str],
        fragments: list[Fragment],
        covered: set[tuple[int, int]],
        added_ends: set[tuple[str, int]],
        depth: int,
        start: int,
        end: int,
        kind: str,
        sym_name: str | None,
    ) -> bool:
        first_child_start = self._first_child_def_line(node, definition_types)
        if first_child_start is None or first_child_start <= start:
            return False
        header_end = first_child_start - 1
        snippet = create_snippet(lines, start, header_end)
        if snippet:
            fragments.append(
                Fragment(
                    id=FragmentId(path=path, start_line=start, end_line=header_end),
                    kind=kind,
                    content=snippet,
                    identifiers=extract_identifiers(snippet, profile="code"),
                    symbol_name=sym_name,
                )
            )
            covered.add((start, header_end))
        added_ends.add((kind, end))
        self._recurse_children(node, code_bytes, path, lines, definition_types, fragments, covered, added_ends, depth)
        return True

    @staticmethod
    def _add_leaf_definition(
        path: Path,
        lines: list[str],
        start: int,
        end: int,
        kind: str,
        sym_name: str | None,
        fragments: list[Fragment],
        covered: set[tuple[int, int]],
        added_ends: set[tuple[str, int]],
    ) -> None:
        if end - start + 1 < MIN_FRAGMENT_LINES:
            return
        snippet = create_snippet(lines, start, end)
        if snippet is None:
            return
        fragments.append(
            Fragment(
                id=FragmentId(path=path, start_line=start, end_line=end),
                kind=kind,
                content=snippet,
                identifiers=extract_identifiers(snippet, profile="code"),
                symbol_name=sym_name,
            )
        )
        covered.add((start, end))
        added_ends.add((kind, end))

    @staticmethod
    def _find_body_node(node: Node) -> Node | None:
        for field in _BODY_FIELD_NAMES:
            child = node.child_by_field_name(field)
            if child is not None:
                return child
        for child in node.children:
            if child.type in ("block", "statement_block", "compound_statement", "function_body"):
                return child
        return None

    @staticmethod
    def _emit_chunk(
        path: Path,
        lines: list[str],
        start: int,
        end: int,
        parent_symbol: str | None,
        fragments: list[Fragment],
        covered: set[tuple[int, int]],
    ) -> None:
        if end < start:
            return
        snippet = create_snippet(lines, start, end)
        if snippet and end - start + 1 >= MIN_FRAGMENT_LINES:
            fragments.append(
                Fragment(
                    id=FragmentId(path=path, start_line=start, end_line=end),
                    kind="chunk",
                    content=snippet,
                    identifiers=extract_identifiers(snippet, profile="code"),
                    symbol_name=f"{parent_symbol}[{start}]" if parent_symbol else None,
                )
            )
            covered.add((start, end))

    @staticmethod
    def _create_sub_fragments(
        node: Node,
        path: Path,
        lines: list[str],
        parent_symbol: str | None,
        fragments: list[Fragment],
        covered: set[tuple[int, int]],
        _depth: int = 0,
    ) -> None:
        if _depth > _MAX_SUB_DEPTH:
            return
        body = TreeSitterStrategy._find_body_node(node)
        if body is None:
            return
        children = [c for c in body.named_children if c.end_point[0] - c.start_point[0] >= 0]
        if len(children) < 2:
            return

        chunk_start_line = children[0].start_point[0] + 1
        chunk_end_line = children[0].end_point[0] + 1

        for child in children[1:]:
            child_start = child.start_point[0] + 1
            child_end = child.end_point[0] + 1
            if child_end - chunk_start_line + 1 > _SUB_FRAGMENT_TARGET_LINES:
                TreeSitterStrategy._emit_chunk(path, lines, chunk_start_line, chunk_end_line, parent_symbol, fragments, covered)
                chunk_start_line = child_start
                chunk_end_line = child_end
            else:
                chunk_end_line = child_end

        TreeSitterStrategy._emit_chunk(path, lines, chunk_start_line, chunk_end_line, parent_symbol, fragments, covered)

    def _recurse_children(
        self,
        node: Node,
        code_bytes: bytes,
        path: Path,
        lines: list[str],
        definition_types: set[str],
        fragments: list[Fragment],
        covered: set[tuple[int, int]],
        added_ends: set[tuple[str, int]],
        depth: int,
    ) -> None:
        for child in node.children:
            self._extract_definitions(child, code_bytes, path, lines, definition_types, fragments, covered, added_ends, depth + 1)

    def _first_child_def_line(self, node: Node, definition_types: set[str], depth: int = 0) -> int | None:
        if depth > 3:
            return None
        for child in node.children:
            if child.type in definition_types:
                return int(child.start_point[0]) + 1
            result = self._first_child_def_line(child, definition_types, depth + 1)
            if result is not None:
                return result
        return None

    def _node_type_to_kind(self, node_type: str, node: Node | None = None) -> str:
        if node_type == "decorated_definition" and node is not None:
            return self._decorated_definition_kind(node)

        for keywords, kind in _NODE_TYPE_KEYWORDS:
            if any(kw in node_type for kw in keywords):
                return kind
        return "definition"

    def _decorated_definition_kind(self, node: Node) -> str:
        for child in node.children:
            if child.type in {"function_definition", "async_function_definition"}:
                return "function"
            if child.type == "class_definition":
                return "class"
        return "function"

    _FUNCTION_CHILD_TYPES = frozenset({"arrow_function", "function", "generator_function"})

    @staticmethod
    def _has_function_child(node: Node) -> bool:
        for child in node.children:
            if child.type in TreeSitterStrategy._FUNCTION_CHILD_TYPES:
                return True
        return False

    @staticmethod
    def _unwrap_decorated(node: Node) -> Node:
        if node.type != "decorated_definition":
            return node
        for child in node.children:
            if child.type in {"function_definition", "class_definition", "async_function_definition"}:
                return child
        return node

    @staticmethod
    def _unwrap_declarator(name_node: Node) -> Node:
        while name_node.type in ("pointer_declarator", "function_declarator"):
            inner = name_node.child_by_field_name("declarator")
            if inner is None:
                break
            name_node = inner
        return name_node

    @classmethod
    def _extract_symbol_name(cls, node: Node) -> str | None:
        node = cls._unwrap_decorated(node)
        for field_name in ("name", "declarator", "type"):
            name_node = node.child_by_field_name(field_name)
            if name_node is None:
                continue
            name_node = cls._unwrap_declarator(name_node)
            if name_node.type == "identifier" or name_node.named_child_count == 0:
                return name_node.text.decode("utf-8") if name_node.text else None
        return None
