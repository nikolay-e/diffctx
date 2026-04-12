from __future__ import annotations

from pathlib import Path

from tree_sitter import Language, Node, Parser

from ..languages import TREE_SITTER_LANGUAGES
from ..types import Fragment, FragmentId, extract_identifiers
from .base import MIN_FRAGMENT_LINES, create_code_gap_fragments, create_snippet

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
}


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

        if lang not in _LANG_MODULES:
            raise ValueError(f"Unsupported language: {lang}")

        import importlib

        module_name = _LANG_MODULES[lang]
        try:
            ts_lang_module = importlib.import_module(module_name)
        except ImportError:
            self._parsers[lang] = None
            raise ValueError(f"Grammar unavailable for language: {lang}")

        if lang == "tsx":
            ts_lang = ts_lang_module.language_tsx()
        elif lang == "typescript":
            ts_lang = ts_lang_module.language_typescript()
        elif hasattr(ts_lang_module, "language"):
            ts_lang = ts_lang_module.language()
        else:
            ts_lang = ts_lang_module

        parser = Parser()
        parser.language = Language(ts_lang)
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
