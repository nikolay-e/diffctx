from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path
from typing import Any

from .types import Fragment

_SYMBOL_PATTERNS: dict[str, list[re.Pattern[str]]] = {
    "function": [
        re.compile(r"^\s*(?:async\s+)?def\s+(\w+)\s*\(", re.MULTILINE),  # Python
        re.compile(r"^\s*(?:export\s+)?(?:async\s+)?function\s+(\w+)\s*[\(<]", re.MULTILINE),  # JS/TS
        re.compile(
            r"^\s*(?:export\s+)?(?:const|let|var)\s+(\w+)\s*=\s*(?:async\s+)?(?:\([^)]*\)|\w)\s*=>", re.MULTILINE
        ),  # Arrow
        re.compile(r"^func\s+(?:\([^)]+\)\s+)?(\w+)\s*[\(\[]", re.MULTILINE),  # Go
        re.compile(r"^\s*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)\s*[\(<]", re.MULTILINE),  # Rust
        re.compile(r"^\s*(?:(?:public|private|protected|static)\s+)*\w[\w<>\[\],]*\s+(\w+)\s*\(", re.MULTILINE),  # Java/C#
    ],
    "class": [
        re.compile(r"^\s*class\s+(\w+)\s*[:\({\s]", re.MULTILINE),  # Python/JS/TS/Java
        re.compile(r"^\s*(?:export\s+)?(?:abstract\s+)?class\s+(\w+)", re.MULTILINE),  # TS/Java
    ],
    "struct": [
        re.compile(r"^\s*(?:pub\s+)?struct\s+(\w+)", re.MULTILINE),  # Rust/Go
        re.compile(r"^\s*type\s+(\w+)\s+struct\s*\{", re.MULTILINE),  # Go
    ],
    "interface": [
        re.compile(r"^\s*(?:export\s+)?interface\s+(\w+)", re.MULTILINE),  # TS/Java
        re.compile(r"^\s*type\s+(\w+)\s+interface\s*\{", re.MULTILINE),  # Go
        re.compile(r"^\s*(?:pub\s+)?trait\s+(\w+)", re.MULTILINE),  # Rust
    ],
    "enum": [
        re.compile(r"^\s*(?:pub\s+)?enum\s+(\w+)", re.MULTILINE),  # Rust/Java/TS
        re.compile(r"^\s*class\s+(\w+)\s*\(.*Enum\)", re.MULTILINE),  # Python Enum
    ],
    "impl": [
        re.compile(r"^\s*impl(?:<[^>]+>)?\s+(\w+)", re.MULTILINE),  # Rust
    ],
    "type": [
        re.compile(r"^\s*(?:export\s+)?type\s+(\w+)", re.MULTILINE),  # TS
        re.compile(r"^\s*type\s+(\w+)\s", re.MULTILINE),  # Go
    ],
    "module": [
        re.compile(r"^\s*(?:pub\s+)?mod\s+(\w+)", re.MULTILINE),  # Rust
        re.compile(r"^\s*package\s+(\w+)", re.MULTILINE),  # Go/Java
    ],
    "section": [
        re.compile(r"^#{1,6}\s+(\S[^\n]*)$", re.MULTILINE),
    ],
}


def _extract_symbol(frag: Fragment) -> str | None:
    patterns = _SYMBOL_PATTERNS.get(frag.kind)
    if not patterns:
        return None
    for pattern in patterns:
        match = pattern.search(frag.content)
        if match:
            result = match.group(1).strip()
            return result[:50] if frag.kind == "section" else result
    return None


def _get_relative_path(frag: Fragment, repo_root: Path) -> Path:
    if not frag.path.is_absolute():
        return frag.path
    try:
        return frag.path.relative_to(repo_root)
    except ValueError:
        return frag.path


def _group_by_path(selected: list[Fragment], repo_root: Path) -> dict[Path, list[Fragment]]:
    by_path: dict[Path, list[Fragment]] = defaultdict(list)
    for frag in selected:
        by_path[_get_relative_path(frag, repo_root)].append(frag)
    return by_path


def _create_fragment_entry(frag: Fragment, path_str: str) -> dict[str, Any]:
    entry: dict[str, Any] = {
        "path": path_str,
        "lines": f"{frag.start_line}-{frag.end_line}",
        "kind": frag.kind,
    }
    symbol = frag.symbol_name or _extract_symbol(frag)
    if symbol:
        entry["symbol"] = symbol
    if frag.content:
        entry["content"] = frag.content
    return entry


def _root_display_name(repo_root: Path) -> str:
    resolved = repo_root.resolve()
    return resolved.name or str(resolved)


def build_diff_context_output(repo_root: Path, selected: list[Fragment]) -> dict[str, Any]:
    by_path = _group_by_path(selected, repo_root)
    fragments_out: list[dict[str, Any]] = []

    for rel_path in sorted(by_path.keys()):
        frags = sorted(by_path[rel_path], key=lambda f: f.start_line)
        path_str = rel_path.as_posix()
        for frag in frags:
            fragments_out.append(_create_fragment_entry(frag, path_str))

    return {
        "name": _root_display_name(repo_root),
        "type": "diff_context",
        "fragment_count": len(fragments_out),
        "fragments": fragments_out,
    }
