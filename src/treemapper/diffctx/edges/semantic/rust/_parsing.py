from __future__ import annotations

import logging
import re
from pathlib import Path

import tree_sitter_rust
from tree_sitter import Language, Node, Parser, Tree

logger = logging.getLogger(__name__)

_LANG = Language(tree_sitter_rust.language())

_RUST_TYPE_REF_RE = re.compile(r"(?<![a-z_])([A-Z]\w*)\b")
_RUST_FN_CALL_RE = re.compile(r"(?<!\w)([a-z_][a-z0-9_]*)\s?!?\s?\(")
_RUST_PATH_CALL_RE = re.compile(r"([a-z_][a-z0-9_]*)::([a-z_][a-z0-9_]*|[A-Z]\w*)")

_RUST_COMMON_TYPES = frozenset(
    {
        "String",
        "Vec",
        "Option",
        "Result",
        "Box",
        "Arc",
        "Rc",
        "Some",
        "None",
        "Ok",
        "Err",
        "Self",
        "HashMap",
        "HashSet",
        "BTreeMap",
        "BTreeSet",
        "Cow",
        "Pin",
        "PhantomData",
    }
)

_RUST_BUILTIN_MACROS = frozenset(
    {
        "println",
        "print",
        "eprintln",
        "eprint",
        "format",
        "vec",
        "assert",
        "assert_eq",
        "assert_ne",
        "debug_assert",
        "debug_assert_eq",
        "debug_assert_ne",
        "panic",
        "todo",
        "unimplemented",
        "unreachable",
        "cfg",
        "env",
        "file",
        "line",
        "column",
        "stringify",
        "concat",
        "include",
        "include_str",
        "include_bytes",
        "write",
        "writeln",
    }
)

_RUST_KEYWORDS = frozenset(
    {
        "if",
        "for",
        "while",
        "match",
        "return",
        "unsafe",
        "loop",
        "break",
        "continue",
        "else",
        "where",
        "as",
        "in",
        "ref",
        "mut",
        "pub",
        "fn",
        "let",
        "const",
        "static",
        "move",
        "async",
        "await",
        "dyn",
        "impl",
        "trait",
        "struct",
        "enum",
        "type",
        "use",
        "mod",
        "crate",
        "self",
        "super",
    }
)

_STRIP_PREFIX = {"crate", "self", "super"}


def _get_parser() -> Parser:
    return Parser(_LANG)


def _parse_tree(content: str) -> Tree | None:
    try:
        return _get_parser().parse(content.encode("utf-8"))
    except Exception:
        logger.debug("tree-sitter failed to parse Rust content")
        return None


def is_rust_file(path: Path) -> bool:
    return path.suffix.lower() == ".rs"


_PATH_IDENT_TYPES = frozenset({"identifier", "crate", "self", "super"})
_USE_LIST_PUNCTUATION = frozenset({"{", "}", ","})


def _extract_scoped_identifier_path(node: Node) -> str:
    parts = [c.text.decode() for c in node.children if c.type in _PATH_IDENT_TYPES]
    return "::".join(parts)


def _collect_scoped_use_list(node: Node) -> list[str]:
    scope_parts: list[str] = []
    use_list = None
    for child in node.children:
        if child.type == "use_list":
            use_list = child
        elif child.type in _PATH_IDENT_TYPES:
            scope_parts.append(child.text.decode())
        elif child.type == "scoped_identifier":
            scope_parts = [c.text.decode() for c in child.children if c.type in _PATH_IDENT_TYPES]
    scope = "::".join(scope_parts)
    if use_list:
        results = []
        for child in use_list.children:
            if child.type in _USE_LIST_PUNCTUATION:
                continue
            for p in _collect_use_paths(child):
                results.append(f"{scope}::{p}" if scope else p)
        return results
    return [scope] if scope else []


def _collect_use_paths(node: Node, prefix: str = "") -> list[str]:
    ntype = node.type
    if ntype in _PATH_IDENT_TYPES:
        name = node.text.decode()
        return [f"{prefix}::{name}" if prefix else name]
    if ntype == "scoped_identifier":
        return [_extract_scoped_identifier_path(node)]
    if ntype == "scoped_use_list":
        return _collect_scoped_use_list(node)
    if ntype == "use_list":
        return [p for c in node.children if c.type not in _USE_LIST_PUNCTUATION for p in _collect_use_paths(c, prefix)]
    if ntype == "use_as_clause":
        for child in node.children:
            if child.type in ("identifier", "scoped_identifier"):
                return _collect_use_paths(child, prefix)
        return []
    if ntype == "use_wildcard":
        return [f"{prefix}::*" if prefix else "*"]
    return []


def _strip_crate_prefix(path: str) -> str:
    parts = path.split("::")
    while parts and parts[0] in _STRIP_PREFIX:
        parts = parts[1:]
    return "::".join(parts)


def extract_uses(content: str) -> set[str]:
    tree = _parse_tree(content)
    if tree is None:
        return set()
    uses: set[str] = set()
    for node in tree.root_node.children:
        if node.type != "use_declaration":
            continue
        for child in node.children:
            if child.type in ("use", ";", "visibility_modifier"):
                continue
            for raw_path in _collect_use_paths(child):
                path = _strip_crate_prefix(raw_path)
                if path:
                    uses.add(path)
                    parts = path.split("::")
                    if len(parts) > 1:
                        uses.add(parts[0])
    return uses


def extract_mods(content: str) -> set[str]:
    tree = _parse_tree(content)
    if tree is None:
        return set()
    mods: set[str] = set()
    for node in tree.root_node.children:
        if node.type != "mod_item":
            continue
        for child in node.children:
            if child.type == "identifier":
                mods.add(child.text.decode())
                break
    return mods


def extract_trait_impls(content: str) -> list[tuple[str, str]]:
    tree = _parse_tree(content)
    if tree is None:
        return []
    result: list[tuple[str, str]] = []
    for node in tree.root_node.children:
        if node.type != "impl_item":
            continue
        has_for = any(c.type == "for" for c in node.children)
        if not has_for:
            continue
        trait_name = None
        impl_type = None
        before_for = True
        for child in node.children:
            if child.type == "for":
                before_for = False
            elif child.type in ("type_identifier", "generic_type"):
                name = _type_name_from_node(child)
                if name and before_for:
                    trait_name = name
                elif name:
                    impl_type = name
        if trait_name and impl_type:
            result.append((trait_name, impl_type))
    return result


def extract_pub_uses(content: str) -> list[str]:
    tree = _parse_tree(content)
    if tree is None:
        return []
    result: list[str] = []
    for node in tree.root_node.children:
        if node.type != "use_declaration":
            continue
        is_pub = any(c.type == "visibility_modifier" for c in node.children)
        if not is_pub:
            continue
        for child in node.children:
            if child.type in ("use", ";", "visibility_modifier"):
                continue
            for raw_path in _collect_use_paths(child):
                path = _strip_crate_prefix(raw_path)
                if path:
                    result.append(path)
    return result


def _type_name_from_node(node: Node) -> str | None:
    if node.type == "type_identifier":
        return str(node.text.decode())
    if node.type == "generic_type":
        for child in node.children:
            if child.type == "type_identifier":
                return str(child.text.decode())
    return None


def _extract_impl_target_type(impl_node: Node) -> str | None:
    has_for = any(c.type == "for" for c in impl_node.children)
    if has_for:
        after_for = False
        for child in impl_node.children:
            if child.type == "for":
                after_for = True
            elif after_for and child.type in ("type_identifier", "generic_type"):
                return _type_name_from_node(child)
    else:
        for child in impl_node.children:
            if child.type in ("type_identifier", "generic_type"):
                return _type_name_from_node(child)
    return None


def extract_definitions(content: str) -> tuple[set[str], set[str]]:
    tree = _parse_tree(content)
    if tree is None:
        return set(), set()
    funcs: set[str] = set()
    types: set[str] = set()
    for node in tree.root_node.children:
        ntype = node.type
        if ntype == "function_item":
            for child in node.children:
                if child.type == "identifier":
                    funcs.add(child.text.decode())
                    break
        elif ntype in ("struct_item", "enum_item", "trait_item", "type_item"):
            for child in node.children:
                if child.type == "type_identifier":
                    types.add(child.text.decode())
                    break
        elif ntype == "impl_item":
            impl_type = _extract_impl_target_type(node)
            if impl_type:
                types.add(impl_type)
    return funcs, types


def extract_references(content: str) -> tuple[set[str], set[str], set[tuple[str, str]]]:
    type_refs = {m.group(1) for m in _RUST_TYPE_REF_RE.finditer(content) if m.group(1) not in _RUST_COMMON_TYPES}
    fn_calls = {
        m.group(1)
        for m in _RUST_FN_CALL_RE.finditer(content)
        if m.group(1) not in _RUST_KEYWORDS and m.group(1) not in _RUST_BUILTIN_MACROS
    }
    path_calls = {(m.group(1), m.group(2)) for m in _RUST_PATH_CALL_RE.finditer(content)}
    return type_refs, fn_calls, path_calls


DISCOVERY_MAX_DEPTH = 2


def stem_to_mod_name(path: Path) -> str:
    stem = path.stem.lower()
    if stem in {"mod", "lib"}:
        return path.parent.name.lower()
    return stem


def read_cached(path: Path, cache: dict[Path, str] | None) -> str | None:
    if cache is not None and path in cache:
        return cache[path]
    try:
        content = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return None
    if cache is not None:
        cache[path] = content
    return content
