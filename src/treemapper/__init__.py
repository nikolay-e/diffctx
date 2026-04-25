from __future__ import annotations

import io
import logging
from pathlib import Path
from typing import Any

from .diffctx import build_diff_context, build_project_graph
from .ignore import get_ignore_specs, get_whitelist_spec
from .tree import TreeBuildContext, build_tree
from .version import __version__
from .writer import write_tree_json, write_tree_markdown, write_tree_text, write_tree_yaml

logging.getLogger("treemapper").addHandler(logging.NullHandler())

__all__ = [
    "__version__",
    "build_diff_context",
    "build_project_graph",
    "map_directory",
    "to_json",
    "to_markdown",
    "to_text",
    "to_yaml",
]


def _root_display_name(user_path: str | Path, resolved: Path) -> str:
    original_name = Path(user_path).name
    if original_name:
        return original_name
    return str(resolved)


def _resolve_path_if_exists(path: str | Path | None, label: str) -> Path | None:
    if path is None:
        return None
    resolved = Path(path).resolve()
    if not resolved.is_file():
        raise FileNotFoundError(f"{label} '{path}' does not exist")
    return resolved


def map_directory(
    path: str | Path,
    *,
    max_depth: int | None = None,
    no_content: bool = False,
    max_file_bytes: int | None = None,
    ignore_file: str | Path | None = None,
    no_default_ignores: bool = False,
    whitelist_file: str | Path | None = None,
) -> dict[str, Any]:
    root_dir = Path(path).resolve()
    if not root_dir.is_dir():
        raise ValueError(f"'{path}' is not a directory")

    ignore_path = _resolve_path_if_exists(ignore_file, "Ignore file")
    whitelist_path = _resolve_path_if_exists(whitelist_file, "Whitelist file")

    ctx = TreeBuildContext(
        base_dir=root_dir,
        combined_spec=get_ignore_specs(root_dir, ignore_path, no_default_ignores, None),
        output_file=None,
        max_depth=max_depth,
        no_content=no_content,
        max_file_bytes=max_file_bytes,
        whitelist_spec=get_whitelist_spec(whitelist_path, root_dir),
    )

    return {
        "name": _root_display_name(path, root_dir),
        "type": "directory",
        "children": build_tree(root_dir, ctx),
    }


def to_yaml(tree: dict[str, Any]) -> str:
    buf = io.StringIO()
    write_tree_yaml(buf, tree)
    return buf.getvalue()


def to_json(tree: dict[str, Any]) -> str:
    buf = io.StringIO()
    write_tree_json(buf, tree)
    return buf.getvalue()


def to_text(tree: dict[str, Any]) -> str:
    buf = io.StringIO()
    write_tree_text(buf, tree)
    return buf.getvalue()


def to_markdown(tree: dict[str, Any]) -> str:
    buf = io.StringIO()
    write_tree_markdown(buf, tree)
    return buf.getvalue()
