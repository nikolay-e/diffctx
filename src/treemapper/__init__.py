import io
from pathlib import Path
from typing import Any, Dict, Optional, Union

from .ignore import get_ignore_specs
from .tree import TreeBuildContext, build_tree
from .version import __version__
from .writer import write_tree_json, write_tree_text, write_tree_yaml

__all__ = [
    "__version__",
    "map_directory",
    "to_yaml",
    "to_json",
    "to_text",
]


def map_directory(
    path: Union[str, Path],
    *,
    max_depth: Optional[int] = None,
    no_content: bool = False,
    max_file_bytes: Optional[int] = None,
    ignore_file: Optional[Union[str, Path]] = None,
    no_default_ignores: bool = False,
) -> Dict[str, Any]:
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


def to_yaml(tree: Dict[str, Any]) -> str:
    buf = io.StringIO()
    write_tree_yaml(buf, tree)
    return buf.getvalue()


def to_json(tree: Dict[str, Any]) -> str:
    buf = io.StringIO()
    write_tree_json(buf, tree)
    return buf.getvalue()


def to_text(tree: Dict[str, Any]) -> str:
    buf = io.StringIO()
    write_tree_text(buf, tree)
    return buf.getvalue()
