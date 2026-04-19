from __future__ import annotations

import ast
import warnings
from pathlib import Path

from ._parsing import _INIT_PY

_REEXPORT_MAX_DEPTH = 3


def _resolve_import_target_dir(node: ast.ImportFrom, pkg_dir: Path, repo_root: Path | None) -> Path | None:
    source_module = node.module or ""
    if node.level and node.level > 0:
        target_dir = pkg_dir
        for _ in range(node.level - 1):
            target_dir = target_dir.parent
        if source_module:
            target_dir = target_dir / Path(*source_module.split("."))
        return target_dir
    if source_module and repo_root:
        return repo_root / Path(*source_module.split("."))
    return None


def _find_module_file(target_dir: Path) -> Path | None:
    source_as_file = target_dir.with_suffix(".py")
    if source_as_file.is_file():
        return source_as_file
    source_as_pkg = target_dir / _INIT_PY
    if source_as_pkg.is_file():
        return source_as_pkg
    for ext in (".pyi", ".pyw"):
        candidate = target_dir.with_suffix(ext)
        if candidate.is_file():
            return candidate
    return None


def _process_reexport_alias(
    alias: ast.alias,
    resolved_path: Path,
    repo_root: Path | None,
    file_cache: dict[Path, str] | None,
    result: dict[str, Path],
    depth: int,
) -> None:
    name = alias.asname or alias.name
    if name == "*":
        result[f"*:{resolved_path}"] = resolved_path
        return
    result[name] = resolved_path
    if resolved_path.name == _INIT_PY:
        nested = _resolve_init_reexports(resolved_path, repo_root, file_cache, depth + 1)
        if name in nested:
            result[name] = nested[name]


def _resolve_init_reexports(
    init_path: Path,
    repo_root: Path | None,
    file_cache: dict[Path, str] | None = None,
    _depth: int = 0,
) -> dict[str, Path]:
    if _depth >= _REEXPORT_MAX_DEPTH:
        return {}

    content = file_cache.get(init_path) if file_cache else None
    if content is None:
        try:
            content = init_path.read_text(errors="replace")
        except OSError:
            return {}

    try:
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", SyntaxWarning)
            tree = ast.parse(content)
    except SyntaxError:
        return {}

    result: dict[str, Path] = {}
    pkg_dir = init_path.parent

    for node in ast.walk(tree):
        if not isinstance(node, ast.ImportFrom) or not node.names:
            continue
        target_dir = _resolve_import_target_dir(node, pkg_dir, repo_root)
        if target_dir is None:
            continue
        resolved_path = _find_module_file(target_dir)
        if resolved_path is None:
            continue
        for alias in node.names:
            _process_reexport_alias(alias, resolved_path, repo_root, file_cache, result, _depth)

    return result
