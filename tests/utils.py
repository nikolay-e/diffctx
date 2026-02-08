# tests/utils.py
from __future__ import annotations

from collections.abc import Hashable
from pathlib import Path
from typing import Any

import pytest
import yaml


def load_yaml(path: Path) -> dict[str, Any]:
    try:
        with path.open("r", encoding="utf-8") as f:
            result = yaml.load(f, Loader=yaml.SafeLoader)
            return result if result is not None else {}
    except FileNotFoundError:
        pytest.fail(f"Output YAML file not found: {path}")
        return {}
    except Exception as e:
        pytest.fail(f"Failed to load or parse YAML file {path}: {e}")
        return {}


def get_all_files_in_tree(node: dict[str, Any]) -> set[str]:
    names: set[str] = set()
    if not isinstance(node, dict) or "name" not in node:
        return names
    names.add(node["name"])
    if "children" in node and isinstance(node["children"], list):
        for child in node["children"]:
            if isinstance(child, dict):
                names.update(get_all_files_in_tree(child))
    return names


def find_node_by_path(tree: dict[str, Any], path_segments: list[str]) -> dict[str, Any] | None:
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
