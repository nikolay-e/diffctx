from __future__ import annotations

import logging
import re
from pathlib import Path

import tree_sitter_go
from tree_sitter import Language, Node, Parser, Tree

logger = logging.getLogger(__name__)

_LANG = Language(tree_sitter_go.language())

_GO_FUNC_CALL_RE = re.compile(r"\b([a-zA-Z_]\w*)\s*\(")
_GO_KEYWORDS = frozenset(
    {
        "if",
        "for",
        "range",
        "switch",
        "select",
        "return",
        "go",
        "defer",
        "func",
        "type",
        "var",
        "const",
        "map",
        "make",
        "new",
        "append",
        "len",
        "cap",
        "copy",
        "delete",
        "close",
        "panic",
        "recover",
        "print",
        "println",
    }
)
_GO_TYPE_REF_RE = re.compile(r"\*?([A-Z]\w*)\b")
_GO_COMMON_TYPES = frozenset(
    {
        "Bool",
        "String",
        "Error",
        "Reader",
        "Writer",
        "Handler",
        "Server",
        "Client",
        "Request",
        "Response",
        "Context",
        "Logger",
        "Config",
        "Options",
        "Result",
        "Status",
        "Mutex",
        "Group",
    }
)
_GO_PKG_CALL_RE = re.compile(r"\b(\w+)\.([A-Z]\w*)")
_GO_EMBED_RE = re.compile(r"//go:embed\s+(\S+)", re.MULTILINE)


def _get_parser() -> Parser:
    return Parser(_LANG)


def _parse_tree(content: str) -> Tree | None:
    try:
        return _get_parser().parse(content.encode("utf-8"))
    except Exception:
        logger.debug("tree-sitter failed to parse Go content")
        return None


def _extract_imports(content: str) -> set[str]:
    tree = _parse_tree(content)
    if tree is None:
        return set()
    imports: set[str] = set()
    for node in tree.root_node.children:
        if node.type != "import_declaration":
            continue
        _collect_import_paths(node, imports)
    return imports


def _collect_import_paths(node: Node, imports: set[str]) -> None:
    for child in node.children:
        if child.type == "import_spec_list":
            for spec in child.children:
                if spec.type == "import_spec":
                    _add_import_from_spec(spec, imports)
        elif child.type == "import_spec":
            _add_import_from_spec(child, imports)
        elif child.type == "interpreted_string_literal":
            imports.add(child.text.decode().strip('"'))


def _add_import_from_spec(spec: Node, imports: set[str]) -> None:
    for child in spec.children:
        if child.type == "interpreted_string_literal":
            imports.add(child.text.decode().strip('"'))
            return


def _extract_definitions(content: str) -> tuple[set[str], set[str]]:
    tree = _parse_tree(content)
    if tree is None:
        return set(), set()
    funcs: set[str] = set()
    types: set[str] = set()
    for node in tree.root_node.children:
        if node.type == "function_declaration":
            for child in node.children:
                if child.type == "identifier":
                    funcs.add(child.text.decode())
                    break
        elif node.type == "method_declaration":
            for child in node.children:
                if child.type == "field_identifier":
                    funcs.add(child.text.decode())
                    break
        elif node.type == "type_declaration":
            for spec in node.children:
                if spec.type == "type_spec":
                    for child in spec.children:
                        if child.type == "type_identifier":
                            types.add(child.text.decode())
                            break
    return funcs, types


def _extract_references(content: str) -> tuple[set[str], set[str], set[tuple[str, str]]]:
    func_calls = {m.group(1) for m in _GO_FUNC_CALL_RE.finditer(content) if m.group(1) not in _GO_KEYWORDS}
    type_refs = {
        m.group(1) for m in _GO_TYPE_REF_RE.finditer(content) if m.group(1)[0].isupper() and m.group(1) not in _GO_COMMON_TYPES
    }
    pkg_calls = {(m.group(1), m.group(2)) for m in _GO_PKG_CALL_RE.finditer(content)}
    return func_calls, type_refs, pkg_calls


def _extract_embedded_types(content: str) -> dict[str, set[str]]:
    tree = _parse_tree(content)
    if tree is None:
        return {}
    result: dict[str, set[str]] = {}
    for node in tree.root_node.children:
        if node.type != "type_declaration":
            continue
        for spec in node.children:
            if spec.type != "type_spec":
                continue
            name_node = _find_child_by_type(spec, "type_identifier")
            struct_node = _find_child_by_type(spec, "struct_type")
            if name_node is None or struct_node is None:
                continue
            embeds = _collect_struct_embeds(struct_node)
            if embeds:
                result[name_node.text.decode()] = embeds
    return result


def _find_child_by_type(node: Node, child_type: str) -> Node | None:
    for child in node.children:
        if child.type == child_type:
            return child
    return None


def _collect_struct_embeds(struct_node: Node) -> set[str]:
    embeds: set[str] = set()
    field_list = struct_node.child_by_field_name("field_list") or struct_node
    for field in field_list.children:
        if field.type != "field_declaration":
            continue
        has_field_name = any(c.type == "field_identifier" for c in field.children)
        if has_field_name:
            continue
        for child in field.children:
            if child.type == "type_identifier" and child.text.decode()[0].isupper():
                embeds.add(child.text.decode())
            elif child.type == "pointer_type":
                inner = _find_child_by_type(child, "type_identifier")
                if inner and inner.text.decode()[0].isupper():
                    embeds.add(inner.text.decode())
    return embeds


def _has_init_func(content: str) -> bool:
    tree = _parse_tree(content)
    if tree is None:
        return False
    for node in tree.root_node.children:
        if node.type == "function_declaration":
            for child in node.children:
                if child.type == "identifier" and child.text.decode() == "init":
                    return True
    return False


def _is_go_file(path: Path) -> bool:
    return path.suffix.lower() == ".go"


def _get_package_name_from_content(content: str, path: Path) -> str:
    tree = _parse_tree(content)
    if tree is not None:
        for node in tree.root_node.children:
            if node.type == "package_clause":
                for child in node.children:
                    if child.type == "package_identifier":
                        return str(child.text.decode())
    return path.parent.name


def _resolve_bases(pattern_str: str, parent: Path, repo_root: Path | None) -> list[Path]:
    base_pattern = pattern_str.split("*")[0].rstrip("/")
    candidate_bases = [parent / base_pattern]
    if repo_root:
        candidate_bases.append(repo_root / base_pattern)
    dirs: list[Path] = []
    for base in candidate_bases:
        try:
            dirs.append(base.resolve())
        except (OSError, ValueError):
            pass
    return dirs


def _any_dir_matches(dirs_to_check: set[Path], embed_dirs: list[Path]) -> bool:
    for d in dirs_to_check:
        try:
            resolved = d.resolve()
            if any(resolved == ed or resolved.is_relative_to(ed) for ed in embed_dirs):
                return True
        except (ValueError, OSError):
            continue
    return False
