# tests/utils.py
from pathlib import Path
from typing import Any, Dict, Hashable, List, Optional, Set

import pytest
import yaml


def load_yaml(path: Path) -> Dict[str, Any]:
    """Load YAML file and return its contents."""
    try:
        with path.open("r", encoding="utf-8") as f:
            return yaml.load(f, Loader=yaml.SafeLoader)
    except FileNotFoundError:
        pytest.fail(f"Output YAML file not found: {path}")
    except Exception as e:
        pytest.fail(f"Failed to load or parse YAML file {path}: {e}")


def get_all_files_in_tree(node: Dict[str, Any]) -> Set[str]:
    """Recursively get all file and directory names from the loaded tree structure."""

    names: Set[str] = set()
    if not isinstance(node, dict) or "name" not in node:
        return names
    names.add(node["name"])
    if "children" in node and isinstance(node["children"], list):
        for child in node["children"]:
            if isinstance(child, dict):
                names.update(get_all_files_in_tree(child))
    return names


def find_node_by_path(tree: Dict[str, Any], path_segments: List[str]) -> Optional[Dict[str, Any]]:
    """Find a node in the tree by list of path segments relative to root node."""
    current_node = tree
    for segment in path_segments:
        if current_node is None or "children" not in current_node or not isinstance(current_node["children"], list):
            return None
        found_child = None
        for child in current_node["children"]:
            if isinstance(child, dict) and child.get("name") == segment:
                found_child = child
                break
        if found_child is None:
            return None
        current_node = found_child
    return current_node


def make_hashable(obj: Any) -> Hashable:
    """Recursively convert dicts and lists to hashable tuples."""
    if isinstance(obj, dict):
        return tuple(sorted((k, make_hashable(v)) for k, v in obj.items()))
    if isinstance(obj, list):
        return tuple(make_hashable(item) for item in obj)
    if isinstance(obj, (str, int, float, bool, type(None))):
        return obj
    try:
        hash(obj)
        return obj
    except TypeError:
        return repr(obj)
