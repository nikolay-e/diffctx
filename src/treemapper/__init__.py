from __future__ import annotations

import io
from pathlib import Path
from typing import Any

from .diffctx import build_diff_context
from .ignore import get_ignore_specs
from .tree import TreeBuildContext, build_tree
from .version import __version__
from .writer import write_tree_json, write_tree_markdown, write_tree_text, write_tree_yaml

__all__ = [
    "__version__",
    "build_diff_context",
    "map_directory",
    "to_json",
    "to_markdown",
    "to_md",
    "to_text",
    "to_txt",
    "to_yaml",
]


def map_directory(
    path: str | Path,
    *,
    max_depth: int | None = None,
    no_content: bool = False,
    max_file_bytes: int | None = None,
    ignore_file: str | Path | None = None,
    no_default_ignores: bool = False,
) -> dict[str, Any]:
    root_dir = Path(path).resolve()
    if not root_dir.is_dir():
        raise ValueError(f"'{path}' is not a directory")

    ignore_path = Path(ignore_file).resolve() if ignore_file else None

    ctx = TreeBuildContext(
        base_dir=root_dir,
        combined_spec=get_ignore_specs(root_dir, ignore_path, no_default_ignores, None),
        output_file=None,
        max_depth=max_depth,
        no_content=no_content,
        max_file_bytes=max_file_bytes,
    )

    return {
        "name": root_dir.name,
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


to_md = to_markdown
to_txt = to_text
