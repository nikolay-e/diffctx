import logging
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, List, Optional

import pathspec  # type: ignore

from .ignore import should_ignore


@dataclass
class TreeBuildContext:
    base_dir: Path
    combined_spec: pathspec.PathSpec
    output_file: Optional[Path] = None
    max_depth: Optional[int] = None
    no_content: bool = False
    max_file_bytes: Optional[int] = None

    def is_output_file(self, entry: Path) -> bool:
        if not self.output_file:
            return False
        try:
            return entry.resolve() == self.output_file.resolve()
        except (OSError, RuntimeError):
            return False


def build_tree(dir_path: Path, ctx: TreeBuildContext, current_depth: int = 0) -> List[Dict[str, Any]]:
    if ctx.max_depth is not None and current_depth >= ctx.max_depth:
        return []

    tree: List[Dict[str, Any]] = []

    try:
        for entry in sorted(dir_path.iterdir()):
            node = _process_entry(entry, ctx, current_depth)
            if node:
                tree.append(node)
    except PermissionError:
        logging.warning(f"Permission denied accessing directory {dir_path}")
    except OSError as e:
        logging.warning(f"Error accessing directory {dir_path}: {e}")

    return tree


def _process_entry(entry: Path, ctx: TreeBuildContext, current_depth: int) -> Optional[Dict[str, Any]]:
    try:
        relative_path = entry.relative_to(ctx.base_dir).as_posix()
        is_dir = entry.is_dir()
    except OSError as e:
        logging.warning(f"Could not process path for entry {entry}: {e}")
        return None

    if ctx.is_output_file(entry):
        logging.debug(f"Skipping output file: {entry}")
        return None

    path_to_check = relative_path + "/" if is_dir else relative_path
    if should_ignore(path_to_check, ctx.combined_spec):
        return None

    if entry.is_symlink() or not entry.exists():
        logging.debug(f"Skipping '{path_to_check}': symlink or not exists")
        return None

    return _create_node(entry, ctx, current_depth)


def _create_node(entry: Path, ctx: TreeBuildContext, current_depth: int) -> Optional[Dict[str, Any]]:
    try:
        is_dir = entry.is_dir()
        node: Dict[str, Any] = {"name": entry.name, "type": "directory" if is_dir else "file"}

        if is_dir:
            children = build_tree(entry, ctx, current_depth + 1)
            if children:
                node["children"] = children
        elif not ctx.no_content:
            node["content"] = _read_file_content(entry, ctx.max_file_bytes)

        return node
    except Exception as e:
        logging.error(f"Failed to create node for {entry.name}: {e}")
        return None


def _read_file_content(file_path: Path, max_file_bytes: Optional[int]) -> str:
    try:
        file_size = file_path.stat().st_size

        if max_file_bytes is not None and file_size > max_file_bytes:
            logging.info(f"Skipping large file {file_path.name}: {file_size} bytes > {max_file_bytes} bytes")
            return f"<file too large: {file_size} bytes>"

        if _is_binary_file(file_path):
            logging.debug(f"Detected binary file {file_path.name}")
            return f"<binary file: {file_size} bytes>"

        content = file_path.read_text(encoding="utf-8")
        cleaned = content.replace("\x00", "")
        if cleaned != content:
            logging.warning(f"Removed NULL bytes from content of {file_path.name}")
            content = cleaned

        if not content:
            return ""
        return content if content.endswith("\n") else content + "\n"

    except PermissionError:
        logging.error(f"Could not read {file_path.name}: Permission denied")
        return "<unreadable content>\n"
    except UnicodeDecodeError:
        logging.error(f"Cannot decode {file_path.name} as UTF-8. Marking as unreadable.")
        return "<unreadable content: not utf-8>\n"
    except IOError as e:
        logging.error(f"Could not read {file_path.name}: {e}")
        return "<unreadable content>\n"


def _is_binary_file(file_path: Path, sample_size: int = 8192) -> bool:
    try:
        with file_path.open("rb") as f:
            return b"\x00" in f.read(sample_size)
    except (OSError, IOError):
        return False
