from __future__ import annotations

from pathlib import Path

from tree_sitter import Language, Node, Parser

from ..languages import TREE_SITTER_LANGUAGES
from ..types import Fragment, FragmentId, extract_identifiers
from .base import MIN_FRAGMENT_LINES, create_code_gap_fragments

_TREE_SITTER_LANGS = TREE_SITTER_LANGUAGES

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
    "go": {"function_declaration", "method_declaration", "type_declaration"},
    "rust": {"function_item", "impl_item", "struct_item", "enum_item", "trait_item"},
    "java": {"method_declaration", "class_declaration", "interface_declaration", "enum_declaration"},
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
    "ruby": {"method", "class", "module"},
    "c_sharp": {
        "method_declaration",
        "class_declaration",
        "interface_declaration",
        "struct_declaration",
        "enum_declaration",
        "record_declaration",
        "property_declaration",
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
        self._parsers: dict[str, Parser] = {}

    def _get_parser(self, lang: str) -> Parser:
        if lang in self._parsers:
            return self._parsers[lang]

        if lang not in _LANG_MODULES:
            raise ValueError(f"Unsupported language: {lang}")

        import importlib

        module_name = _LANG_MODULES[lang]
        ts_lang_module = importlib.import_module(module_name)

        if lang == "tsx":
            ts_lang = ts_lang_module.language_tsx()
        elif lang == "typescript":
            ts_lang = ts_lang_module.language_typescript()
        elif hasattr(ts_lang_module, "language"):
            ts_lang = ts_lang_module.language()
        else:
            ts_lang = ts_lang_module

        parser = Parser()
        if isinstance(ts_lang, Language):
            parser.language = ts_lang
        else:
            parser.language = Language(ts_lang)
        self._parsers[lang] = parser
        return parser

    def can_handle(self, path: Path, _content: str) -> bool:
        suffix = path.suffix.lower()
        return suffix in _TREE_SITTER_LANGS

    def fragment(self, path: Path, content: str) -> list[Fragment]:
        suffix = path.suffix.lower()
        lang = _TREE_SITTER_LANGS[suffix]
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

        return fragments if fragments else []

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

        start = node.start_point[0] + 1
        end = node.end_point[0] + 1

        if node.type in definition_types:
            kind = self._node_type_to_kind(node.type, node)

            if (kind, end) in added_ends:
                for child in node.children:
                    self._extract_definitions(
                        child, code_bytes, path, lines, definition_types, fragments, covered, added_ends, depth + 1
                    )
                return

            if end - start + 1 >= MIN_FRAGMENT_LINES:
                snippet = code_bytes[node.start_byte : node.end_byte].decode("utf-8", errors="replace")
                if not snippet.endswith("\n"):
                    snippet += "\n"

                fragments.append(
                    Fragment(
                        id=FragmentId(path=path, start_line=start, end_line=end),
                        kind=kind,
                        content=snippet,
                        identifiers=extract_identifiers(snippet, profile="code"),
                    )
                )
                covered.add((start, end))
                added_ends.add((kind, end))

        for child in node.children:
            self._extract_definitions(child, code_bytes, path, lines, definition_types, fragments, covered, added_ends, depth + 1)

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
