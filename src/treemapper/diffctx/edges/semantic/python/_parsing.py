# pylint: disable=duplicate-code
from __future__ import annotations

import ast
import re
import warnings
from pathlib import Path

from ...base import _strip_source_prefix

_IMPORT_FROM_RE = re.compile(r"^\s*from\s+(\.+)?([\w.]*)\s+import")
_IMPORT_SIMPLE_RE = re.compile(r"^\s*import\s+([\w.]+(?:\s*,\s*[\w.]+)*)")

_INIT_PY = "__init__.py"
_PYTHON_EXTS = {".py", ".pyi", ".pyw"}


def _is_python_file(path: Path) -> bool:
    return path.suffix.lower() in _PYTHON_EXTS


def _add_import_with_prefixes(imports: set[str], imported: str) -> None:
    imports.add(imported)
    parts = imported.split(".")
    for i in range(1, len(parts) + 1):
        imports.add(".".join(parts[:i]))


def _resolve_relative(name: str, source_path: Path, repo_root: Path | None) -> str | None:
    try:
        import importlib.util

        pkg_parts = _strip_source_prefix(list(source_path.parent.parts))
        if pkg_parts and pkg_parts[-1] == "__pycache__":
            pkg_parts = pkg_parts[:-1]
        if repo_root and source_path.is_absolute():
            try:
                source_path = source_path.relative_to(repo_root)
                pkg_parts = _strip_source_prefix(list(source_path.parent.parts))
            except ValueError:
                pass
        package = ".".join(pkg_parts) if pkg_parts else None
        if not package:
            return None
        return importlib.util.resolve_name(name, package)
    except (ImportError, ValueError):
        return None


def _collect_import_from(
    node: ast.ImportFrom,
    imports: set[str],
    source_path: Path | None,
    repo_root: Path | None,
) -> None:
    module = node.module or ""
    if node.level and node.level > 0:
        relative = "." * node.level + module
        if source_path:
            resolved = _resolve_relative(relative, source_path, repo_root)
            if resolved:
                _add_import_with_prefixes(imports, resolved)
    elif module:
        _add_import_with_prefixes(imports, module)
        for alias in node.names:
            if alias.name and alias.name != "*":
                _add_import_with_prefixes(imports, f"{module}.{alias.name}")


def _extract_imports_from_content(content: str, source_path: Path | None = None, repo_root: Path | None = None) -> set[str]:
    imports: set[str] = set()

    try:
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", SyntaxWarning)
            tree = ast.parse(content)
    except SyntaxError:
        return imports

    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                if alias.name:
                    _add_import_with_prefixes(imports, alias.name)
        elif isinstance(node, ast.ImportFrom):
            _collect_import_from(node, imports, source_path, repo_root)
    return imports
