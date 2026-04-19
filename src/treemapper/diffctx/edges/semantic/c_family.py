from __future__ import annotations

import logging
import re
from collections import defaultdict
from pathlib import Path

import tree_sitter_c
import tree_sitter_cpp
from tree_sitter import Language, Node, Parser, Tree

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict

logger = logging.getLogger(__name__)

_C_LANG = Language(tree_sitter_c.language())
_CPP_LANG = Language(tree_sitter_cpp.language())

_C_EXTENSIONS = {".c", ".h"}
_CPP_EXTENSIONS = {".cpp", ".hpp", ".cc", ".cxx", ".hxx", ".c++", ".h++", ".hh", ".ipp", ".tpp"}
_OBJC_EXTENSIONS = {".m", ".mm"}
_ALL_C_FAMILY = _C_EXTENSIONS | _CPP_EXTENSIONS | _OBJC_EXTENSIONS

_INCLUDE_RE = re.compile(r'^\s*#\s*(?:include|import)\s*[<"]([^>"]+)[>"]', re.MULTILINE)

_FUNC_CALL_RE = re.compile(r"\b(\w+)\s*\(")
_TYPE_REF_RE = re.compile(r"\b([A-Z]\w*)\b")

_HEADER_EXTENSIONS = frozenset({".h", ".hpp", ".hh", ".hxx", ".h++"})
_IMPL_EXTENSIONS = frozenset({".c", ".cpp", ".cc", ".cxx", ".c++", ".m", ".mm"})
_MIN_IDENTIFIER_LENGTH = 2

_DISCOVERY_MAX_DEPTH = 2

_C_KEYWORDS = frozenset(
    {
        "if",
        "for",
        "while",
        "switch",
        "case",
        "return",
        "sizeof",
        "typeof",
        "alignof",
        "static_assert",
        "do",
        "else",
        "goto",
        "break",
        "continue",
        "default",
        "register",
        "volatile",
        "extern",
        "typedef",
        "auto",
        "inline",
        "restrict",
        "noexcept",
        "decltype",
        "nullptr",
        "throw",
        "try",
        "catch",
        "delete",
        "new",
        "template",
        "namespace",
        "using",
        "operator",
    }
)

_C_COMMON_MACROS = frozenset(
    {
        "NULL",
        "TRUE",
        "FALSE",
        "BOOL",
        "DWORD",
        "HANDLE",
        "VOID",
        "HRESULT",
        "LPCTSTR",
        "LPCSTR",
        "LPWSTR",
        "INT",
        "UINT",
        "LONG",
        "ULONG",
        "WORD",
        "BYTE",
        "CHAR",
        "SHORT",
        "EOF",
        "SIZE_MAX",
        "INT_MAX",
        "INT_MIN",
    }
)


def _get_parser(path: Path) -> Parser:
    lang = _CPP_LANG if path.suffix.lower() in _CPP_EXTENSIONS else _C_LANG
    return Parser(lang)


def _parse_tree(content: str, path: Path) -> Tree | None:
    try:
        return _get_parser(path).parse(content.encode("utf-8"))
    except Exception:
        logger.debug("tree-sitter failed to parse C/C++ content for %s", path)
        return None


def _is_c_family(path: Path) -> bool:
    return path.suffix.lower() in _ALL_C_FAMILY


def _extract_includes(content: str) -> set[str]:
    includes: set[str] = set()
    for match in _INCLUDE_RE.finditer(content):
        header = match.group(1)
        includes.add(header)
        if "/" in header:
            includes.add(header.split("/")[-1])
    return includes


def _find_child_by_type(node: Node, child_type: str) -> Node | None:
    for child in node.children:
        if child.type == child_type:
            return child
    return None


def _extract_func_name_from_declarator(declarator: Node) -> tuple[str | None, str | None]:
    for child in declarator.children:
        if child.type == "identifier":
            return child.text.decode(), None
        if child.type == "qualified_identifier":
            parts = [c.text.decode() for c in child.children if c.type in ("identifier", "type_identifier")]
            if len(parts) >= 2:
                return parts[-1], parts[-2]
            if parts:
                return parts[0], None
        if child.type == "function_declarator":
            return _extract_func_name_from_declarator(child)
        if child.type == "pointer_declarator":
            return _extract_func_name_from_declarator(child)
        if child.type == "reference_declarator":
            return _extract_func_name_from_declarator(child)
    return None, None


_TYPE_DEFINITION_NODES = frozenset(
    {
        "class_specifier",
        "struct_specifier",
        "enum_specifier",
        "type_definition",
        "alias_declaration",
    }
)


def _add_type_name(child: Node, types: set[str]) -> None:
    name_node = _find_child_by_type(child, "type_identifier")
    if name_node:
        types.add(name_node.text.decode())


def _add_func_definition(child: Node, functions: set[str], types: set[str]) -> None:
    declarator = child.child_by_field_name("declarator")
    if declarator:
        func_name, class_name = _extract_func_name_from_declarator(declarator)
        if func_name and func_name not in _C_KEYWORDS:
            functions.add(func_name)
        if class_name:
            types.add(class_name)


def _walk_definitions(node: Node, functions: set[str], types: set[str], namespaces: set[str]) -> None:
    for child in node.children:
        ntype = child.type
        if ntype == "function_definition":
            _add_func_definition(child, functions, types)
        elif ntype in _TYPE_DEFINITION_NODES:
            _add_type_name(child, types)
        elif ntype == "namespace_definition":
            name_node = _find_child_by_type(child, "identifier")
            if name_node:
                namespaces.add(name_node.text.decode())
            body = child.child_by_field_name("body")
            if body:
                _walk_definitions(body, functions, types, namespaces)


def _extract_definitions(content: str, path: Path | None = None) -> tuple[set[str], set[str], set[str]]:
    if path is None:
        path = Path("unknown.cpp")
    tree = _parse_tree(content, path)
    if tree is None:
        return set(), set(), set()
    functions: set[str] = set()
    types: set[str] = set()
    namespaces: set[str] = set()
    _walk_definitions(tree.root_node, functions, types, namespaces)
    return functions, types, namespaces


def _extract_inheritance(content: str, path: Path | None = None) -> list[tuple[str, set[str]]]:
    if path is None:
        path = Path("unknown.cpp")
    tree = _parse_tree(content, path)
    if tree is None:
        return []
    results: list[tuple[str, set[str]]] = []
    _walk_inheritance(tree.root_node, results)
    return results


def _walk_inheritance(node: Node, results: list[tuple[str, set[str]]]) -> None:
    for child in node.children:
        if child.type in ("class_specifier", "struct_specifier"):
            name_node = _find_child_by_type(child, "type_identifier")
            base_clause = _find_child_by_type(child, "base_class_clause")
            if name_node and base_clause:
                bases = {c.text.decode() for c in base_clause.children if c.type == "type_identifier"}
                if bases:
                    results.append((name_node.text.decode(), bases))
        elif child.type == "namespace_definition":
            body = child.child_by_field_name("body")
            if body:
                _walk_inheritance(body, results)


def _extract_forward_decls(content: str, path: Path | None = None) -> set[str]:
    if path is None:
        path = Path("unknown.cpp")
    tree = _parse_tree(content, path)
    if tree is None:
        return set()
    result: set[str] = set()
    _walk_forward_decls(tree.root_node, result)
    return result


def _walk_forward_decls(node: Node, result: set[str]) -> None:
    for child in node.children:
        if child.type == "declaration":
            has_body = any(c.type == "field_declaration_list" for c in child.children)
            if not has_body:
                for inner in child.children:
                    if inner.type in ("class_specifier", "struct_specifier"):
                        name_node = _find_child_by_type(inner, "type_identifier")
                        if name_node:
                            result.add(name_node.text.decode())
        elif child.type in ("class_specifier", "struct_specifier"):
            if not _find_child_by_type(child, "field_declaration_list"):
                name_node = _find_child_by_type(child, "type_identifier")
                if name_node:
                    result.add(name_node.text.decode())
        elif child.type == "namespace_definition":
            body = child.child_by_field_name("body")
            if body:
                _walk_forward_decls(body, result)


def _extract_friend_decls(content: str, path: Path | None = None) -> set[str]:
    if path is None:
        path = Path("unknown.cpp")
    tree = _parse_tree(content, path)
    if tree is None:
        return set()
    result: set[str] = set()
    _walk_friend_decls(tree.root_node, result)
    return result


def _walk_friend_decls(node: Node, result: set[str]) -> None:
    for child in node.children:
        if child.type == "friend_declaration":
            name_node = _find_child_by_type(child, "type_identifier")
            if name_node:
                result.add(name_node.text.decode())
        elif hasattr(child, "children"):
            body = child.child_by_field_name("body") or child.child_by_field_name("field_declaration_list")
            if body:
                _walk_friend_decls(body, result)


def _extract_references(content: str, own_defs: set[str]) -> tuple[set[str], set[str]]:
    calls: set[str] = set()
    type_refs: set[str] = set()

    for match in _FUNC_CALL_RE.finditer(content):
        name = match.group(1)
        if name in _C_KEYWORDS:
            continue
        if name not in own_defs and not name.startswith("_") and len(name) > _MIN_IDENTIFIER_LENGTH:
            calls.add(name)

    for match in _TYPE_REF_RE.finditer(content):
        name = match.group(1)
        if name in _C_COMMON_MACROS:
            continue
        if name not in own_defs and len(name) > _MIN_IDENTIFIER_LENGTH:
            type_refs.add(name)

    return calls, type_refs


class _CIndex:
    header_to_frags: dict[str, list[FragmentId]]
    func_defs: dict[str, list[FragmentId]]
    type_defs: dict[str, list[FragmentId]]
    frag_own_defs: dict[FragmentId, set[str]]

    def __init__(self) -> None:
        self.header_to_frags = defaultdict(list)
        self.func_defs = defaultdict(list)
        self.type_defs = defaultdict(list)
        self.frag_own_defs = {}


class CFamilyEdgeBuilder(EdgeBuilder):
    weight = 0.70
    include_weight = EDGE_WEIGHTS["c_include"].forward
    call_weight = EDGE_WEIGHTS["c_call"].forward
    type_weight = EDGE_WEIGHTS["c_type"].forward
    inheritance_weight = EDGE_WEIGHTS["c_inheritance"].forward
    forward_decl_weight = EDGE_WEIGHTS["c_forward_decl"].forward
    friend_weight = EDGE_WEIGHTS["c_friend"].forward
    reverse_weight_factor = EDGE_WEIGHTS["c_include"].reverse_factor

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        c_changed = [f for f in changed_files if _is_c_family(f)]
        if not c_changed:
            return []

        changed_set = set(changed_files)
        discovered: set[Path] = set()
        frontier = list(c_changed)

        for _depth in range(_DISCOVERY_MAX_DEPTH):
            hop_found: list[Path] = []

            included_headers = self._collect_included_headers(frontier)
            if included_headers:
                hop_found.extend(self._find_files_for_headers(all_candidate_files, changed_set | discovered, included_headers))

            frontier_names = self._collect_changed_names(frontier)
            if frontier_names:
                hop_found.extend(
                    self._find_files_including_headers(all_candidate_files, changed_set | discovered, frontier_names)
                )

            new_files = [f for f in hop_found if f not in discovered]
            if not new_files:
                break

            discovered.update(new_files)
            frontier = new_files

        return sorted(discovered)

    def _collect_included_headers(self, c_changed: list[Path]) -> set[str]:
        included: set[str] = set()
        for f in c_changed:
            try:
                content = f.read_text(encoding="utf-8")
                for inc in _extract_includes(content):
                    included.add(inc)
                    if "/" in inc:
                        included.add(inc.split("/")[-1])
            except (OSError, UnicodeDecodeError):
                logger.debug("skipping unreadable file: %s", f)
                continue
        return included

    def _find_files_for_headers(self, all_candidate_files: list[Path], changed_set: set[Path], headers: set[str]) -> list[Path]:
        discovered: list[Path] = []
        for candidate in all_candidate_files:
            if candidate in changed_set or not _is_c_family(candidate):
                continue
            if candidate.name in headers:
                discovered.append(candidate)
        return discovered

    def _collect_changed_names(self, c_changed: list[Path]) -> set[str]:
        names: set[str] = set()
        for f in c_changed:
            names.add(f.name)
            names.add(f.stem + ".h")
            names.add(f.stem + ".hpp")
        return names

    def _find_files_including_headers(
        self, all_candidate_files: list[Path], changed_set: set[Path], changed_names: set[str]
    ) -> list[Path]:
        discovered: list[Path] = []
        for candidate in all_candidate_files:
            if candidate in changed_set or not _is_c_family(candidate):
                continue
            if self._includes_any_header(candidate, changed_names):
                discovered.append(candidate)
        return discovered

    def _includes_any_header(self, candidate: Path, changed_names: set[str]) -> bool:
        try:
            content = candidate.read_text(encoding="utf-8")
            includes = _extract_includes(content)
            for inc in includes:
                inc_name = inc.split("/")[-1] if "/" in inc else inc
                if inc_name in changed_names:
                    return True
            return False
        except (OSError, UnicodeDecodeError):
            logger.debug("skipping unreadable file: %s", candidate)
            return False

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        c_frags = [f for f in fragments if _is_c_family(f.path)]
        if not c_frags:
            return {}

        edges: EdgeDict = {}
        idx = self._build_index(c_frags)

        for f in c_frags:
            self._add_fragment_edges(f, idx, edges)

        self._link_header_impl_pairs(c_frags, edges)
        return edges

    def _build_index(self, c_frags: list[Fragment]) -> _CIndex:
        idx = _CIndex()

        for f in c_frags:
            idx.header_to_frags[f.path.name].append(f.id)
            if f.path.stem:
                idx.header_to_frags[f.path.stem + ".h"].append(f.id)
                idx.header_to_frags[f.path.stem + ".hpp"].append(f.id)

            functions, types, _ = _extract_definitions(f.content, f.path)
            idx.frag_own_defs[f.id] = functions | types

            for func in functions:
                idx.func_defs[func].append(f.id)
            for t in types:
                idx.type_defs[t].append(f.id)

        return idx

    def _add_fragment_edges(self, f: Fragment, idx: _CIndex, edges: EdgeDict) -> None:
        self._add_include_edges(f, idx.header_to_frags, edges)

        own_defs = idx.frag_own_defs.get(f.id, set())
        calls, type_refs = _extract_references(f.content, own_defs)

        self._add_call_edges(f.id, calls, idx.func_defs, edges)
        self._add_type_edges(f.id, type_refs, idx.type_defs, edges)
        self._add_inheritance_edges(f, idx.type_defs, edges)
        self._add_forward_decl_edges(f, idx.type_defs, edges)
        self._add_friend_edges(f, idx.type_defs, edges)

    def _add_include_edges(self, f: Fragment, header_to_frags: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        for inc in _extract_includes(f.content):
            inc_name = inc.split("/")[-1] if "/" in inc else inc
            for target_id in header_to_frags.get(inc_name, []):
                if target_id != f.id:
                    self.add_edge(edges, f.id, target_id, self.include_weight)

    def _add_call_edges(
        self,
        src_id: FragmentId,
        calls: set[str],
        func_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for call in calls:
            for def_id in func_defs.get(call, []):
                if def_id != src_id:
                    self.add_edge(edges, src_id, def_id, self.call_weight)

    def _add_type_edges(
        self,
        src_id: FragmentId,
        type_refs: set[str],
        type_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for t in type_refs:
            for def_id in type_defs.get(t, []):
                if def_id != src_id:
                    self.add_edge(edges, src_id, def_id, self.type_weight)

    def _add_inheritance_edges(
        self,
        f: Fragment,
        type_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for _derived, bases in _extract_inheritance(f.content, f.path):
            for base in bases:
                for def_id in type_defs.get(base, []):
                    if def_id != f.id:
                        self.add_edge(edges, f.id, def_id, self.inheritance_weight)

    def _add_forward_decl_edges(
        self,
        f: Fragment,
        type_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for name in _extract_forward_decls(f.content, f.path):
            for def_id in type_defs.get(name, []):
                if def_id != f.id:
                    self.add_edge(edges, f.id, def_id, self.forward_decl_weight)

    def _add_friend_edges(
        self,
        f: Fragment,
        type_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for name in _extract_friend_decls(f.content, f.path):
            for def_id in type_defs.get(name, []):
                if def_id != f.id:
                    self.add_edge(edges, f.id, def_id, self.friend_weight)

    def _link_header_impl_pairs(self, frags: list[Fragment], edges: EdgeDict) -> None:
        by_stem: dict[str, list[Fragment]] = defaultdict(list)
        for f in frags:
            by_stem[f.path.stem.lower()].append(f)

        for _stem, group in by_stem.items():
            if len(group) < 2:
                continue

            headers = [f for f in group if f.path.suffix.lower() in _HEADER_EXTENSIONS]
            impls = [f for f in group if f.path.suffix.lower() in _IMPL_EXTENSIONS]

            for h in headers:
                for impl in impls:
                    self.add_edge(edges, h.id, impl.id, self.weight)
