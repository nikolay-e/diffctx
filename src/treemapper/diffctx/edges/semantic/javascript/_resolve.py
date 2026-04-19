from __future__ import annotations

import re
from pathlib import Path

from ..javascript_semantics import extract_import_sources

_JS_EXTS = {".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".mts", ".cts"}
_TS_EXTS = {".ts", ".tsx", ".mts", ".cts"}
_JSON_EXT = ".json"

_EXPORT_DECL_RE = re.compile(
    r"export\s+(?:const|let|var|function\*?|class|async\s+function|interface|type|enum|abstract\s+class)\s+(\w+)",
    re.MULTILINE,
)
_EXPORT_DEFAULT_NAME_RE = re.compile(
    r"export\s+default\s+(?:(?:class|function\*?|async\s+function)\s+)?(\w+)",
    re.MULTILINE,
)
_EXPORT_LIST_RE = re.compile(r"export\s*\{([^}]+)\}", re.MULTILINE)
_NAMED_IMPORT_NAMES_RE = re.compile(
    r"import\s*(?:type\s*)?\{([^}]+)\}\s*from\s*['\"]",
    re.MULTILINE,
)

_JS_KEYWORDS = frozenset(
    {
        "new",
        "class",
        "function",
        "async",
        "await",
        "return",
        "throw",
        "delete",
        "typeof",
        "instanceof",
        "void",
        "yield",
        "super",
        "this",
        "null",
        "undefined",
        "true",
        "false",
    }
)

_REEXPORT_SOURCE_RE = re.compile(
    r"export\s*(?:\*|\{[^}]*\})\s*from\s*['\"]([^'\"]+)['\"]",
    re.MULTILINE,
)


def _add_name_if_valid(name: str, target: set[str]) -> None:
    if name and len(name) >= 2:
        target.add(name.lower())


def _extract_exports_from_content(content: str, exported: set[str]) -> None:
    for m in _EXPORT_DECL_RE.finditer(content):
        _add_name_if_valid(m.group(1), exported)
    for m in _EXPORT_DEFAULT_NAME_RE.finditer(content):
        captured = m.group(1)
        if captured in _JS_KEYWORDS:
            continue
        _add_name_if_valid(captured, exported)
    for m in _EXPORT_LIST_RE.finditer(content):
        for part in m.group(1).split(","):
            part = part.strip().split(" as ")[0].strip()
            _add_name_if_valid(part, exported)


def _is_js_file(path: Path) -> bool:
    return path.suffix.lower() in _JS_EXTS


def _normalize_import(imp: str, source_path: Path) -> set[str]:
    names: set[str] = set()

    if imp.startswith("."):
        base_dir = source_path.parent
        parts = imp.split("/")
        resolved_parts: list[str] = []

        for part in parts:
            if part == ".":
                continue
            elif part == "..":
                base_dir = base_dir.parent
            else:
                resolved_parts.append(part)

        if resolved_parts:
            base_parts = list(base_dir.parts)
            full_resolved = base_parts + resolved_parts
            names.add("/".join(full_resolved))
            names.add("/".join(resolved_parts))
            names.add(resolved_parts[-1])
    else:
        names.add(imp)
        parts = imp.split("/")
        if len(parts) > 1:
            names.add(parts[-1])

    return names


def _extract_imports_from_content(content: str, source_path: Path) -> set[str]:
    raw_sources = extract_import_sources(content)
    normalized: set[str] = set()
    for source in raw_sources:
        normalized.update(_normalize_import(source, source_path))
    return normalized


def _resolve_relative_import(
    source_file: Path,
    import_source: str,
    candidate_set: set[Path],
) -> Path | None:
    base_dir = source_file.parent
    parts = import_source.split("/")
    resolved = base_dir
    for part in parts:
        if part == ".":
            continue
        elif part == "..":
            resolved = resolved.parent
        else:
            resolved = resolved / part

    for ext in (".ts", ".tsx", ".js", ".jsx", ".mts", ".cts", ".mjs", ".cjs"):
        candidate = resolved.parent / (resolved.name + ext)
        if candidate in candidate_set:
            return candidate

    for index_name in ("index.ts", "index.tsx", "index.js", "index.jsx"):
        candidate = resolved / index_name
        if candidate in candidate_set:
            return candidate

    if resolved in candidate_set:
        return resolved

    return None


def _resolve_absolute_import(
    resolved_path: Path,
    candidate_set: set[Path],
) -> Path | None:
    for ext in (".ts", ".tsx", ".js", ".jsx", ".mts", ".cts", ".mjs", ".cjs"):
        candidate = resolved_path.parent / (resolved_path.name + ext)
        if candidate in candidate_set:
            return candidate

    for index_name in ("index.ts", "index.tsx", "index.js", "index.jsx"):
        candidate = resolved_path / index_name
        if candidate in candidate_set:
            return candidate

    if resolved_path in candidate_set:
        return resolved_path

    return None
