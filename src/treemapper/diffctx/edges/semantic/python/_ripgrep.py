from __future__ import annotations

import os
import shutil
import subprocess
from collections import defaultdict
from pathlib import Path

from ...base import _strip_source_prefix
from ._parsing import _IMPORT_FROM_RE, _IMPORT_SIMPLE_RE, _add_import_with_prefixes

_RG_BIN = None if os.environ.get("DIFFCTX_NO_RIPGREP") else shutil.which("rg")


def _resolve_relative_rg(source_path: Path, repo_root: Path, level: int, module: str) -> str | None:
    try:
        rel = source_path.relative_to(repo_root)
    except ValueError:
        return None
    parts = _strip_source_prefix(list(rel.parent.parts))
    if parts and parts[-1] == "__pycache__":
        parts = parts[:-1]
    if level > len(parts):
        return None
    base_parts = parts[: len(parts) - level + 1]
    if module:
        base_parts.extend(module.split("."))
    return ".".join(base_parts) if base_parts else None


def _parse_rg_line(line: str) -> tuple[Path, str] | None:
    idx = line.find(":")
    if idx < 0:
        return None
    path_str = line[:idx]
    rest = line[idx + 1 :]
    idx2 = rest.find(":")
    content = rest[idx2 + 1 :] if idx2 >= 0 and rest[:idx2].isdigit() else rest
    return Path(path_str), content


def _process_rg_import(stripped: str, path: Path, file_to_imports: dict[Path, set[str]], repo_root: Path) -> None:
    m_from = _IMPORT_FROM_RE.match(stripped)
    m_simple = _IMPORT_SIMPLE_RE.match(stripped)
    if m_simple:
        for name in m_simple.group(1).split(","):
            name = name.split(" as ")[0].strip()
            if name:
                _add_import_with_prefixes(file_to_imports[path], name)
    elif m_from:
        dots, module = m_from.group(1), m_from.group(2)
        if dots:
            resolved = _resolve_relative_rg(path, repo_root, len(dots), module or "")
            if resolved:
                _add_import_with_prefixes(file_to_imports[path], resolved)
        elif module:
            _add_import_with_prefixes(file_to_imports[path], module)


def _build_import_index_rg(
    repo_root: Path,
    candidate_set: set[Path],
) -> dict[Path, set[str]]:
    r = subprocess.run(
        [
            _RG_BIN or "rg",
            "--no-heading",
            "--with-filename",
            r"^\s*(?:from\s+\.*[\w.]*\s+import|import\s+[\w.])",
            "--type",
            "py",
            str(repo_root),
        ],
        capture_output=True,
        text=True,
        timeout=30,
    )
    file_to_imports: dict[Path, set[str]] = defaultdict(set)
    for line in r.stdout.splitlines():
        parsed = _parse_rg_line(line)
        if parsed is None:
            continue
        path, content = parsed
        if path not in candidate_set:
            continue
        stripped = content.strip()
        _process_rg_import(stripped, path, file_to_imports, repo_root)

    return file_to_imports
