# src/treemapper/tree.py
import logging
from pathlib import Path
from typing import Any, Dict, List, Optional

# pathspec doesn't have type stubs
import pathspec  # type: ignore

from .ignore import should_ignore


def build_tree(
    dir_path: Path,
    base_dir: Path,
    combined_spec: pathspec.PathSpec,
    output_file: Optional[Path] = None,
    max_depth: Optional[int] = None,
    current_depth: int = 0,
    no_content: bool = False,
    max_file_bytes: Optional[int] = None,
) -> List[Dict[str, Any]]:
    """Build the directory tree structure."""
    tree: List[Dict[str, Any]] = []

    # Check if we've reached max depth
    if max_depth is not None and current_depth >= max_depth:
        return tree

    try:
        for entry in sorted(dir_path.iterdir()):
            try:
                relative_path = entry.relative_to(base_dir).as_posix()
                is_dir_entry = entry.is_dir()
            except OSError as e:
                logging.warning(f"Could not process path for entry {entry}: {e}")
                continue

            # Always skip the output file, regardless of ignore patterns
            if output_file:
                try:
                    if entry.resolve() == output_file.resolve():
                        logging.debug(f"Skipping output file: {entry}")
                        continue
                except (OSError, RuntimeError):
                    # If we can't resolve paths, continue with other checks
                    pass

            if is_dir_entry:
                relative_path_check = relative_path + "/"
            else:
                relative_path_check = relative_path

            if should_ignore(relative_path_check, combined_spec):
                continue

            if not entry.exists() or entry.is_symlink():
                logging.debug(f"Skipping '{relative_path_check}': not exists or is symlink")
                continue

            node = create_node(entry, base_dir, combined_spec, output_file, max_depth, current_depth, no_content, max_file_bytes)
            if node:
                tree.append(node)

    except PermissionError:
        logging.warning(f"Permission denied accessing directory {dir_path}")
    except OSError as e:
        logging.warning(f"Error accessing directory {dir_path}: {e}")

    return tree


def _is_binary_file(file_path: Path, sample_size: int = 8192) -> bool:
    """
    Check if a file is binary by reading a small sample and checking for NUL bytes.
    Returns True if the file appears to be binary.
    """
    try:
        with file_path.open("rb") as f:
            chunk = f.read(sample_size)
            return b"\x00" in chunk
    except (OSError, IOError):
        return False


def create_node(
    entry: Path,
    base_dir: Path,
    combined_spec: pathspec.PathSpec,
    output_file: Optional[Path] = None,
    max_depth: Optional[int] = None,
    current_depth: int = 0,
    no_content: bool = False,
    max_file_bytes: Optional[int] = None,
) -> Optional[Dict[str, Any]]:
    """Create a node for the tree structure. Returns None if node creation fails."""
    try:
        node_type = "directory" if entry.is_dir() else "file"

        node: Dict[str, Any] = {"name": entry.name, "type": node_type}

        if node_type == "directory":
            children = build_tree(
                entry, base_dir, combined_spec, output_file, max_depth, current_depth + 1, no_content, max_file_bytes
            )
            if children:
                node["children"] = children
        elif node_type == "file":
            # Skip content if --no-content flag is set
            if no_content:
                return node

            node_content: Optional[str] = None
            try:
                # Check file size first
                file_size = entry.stat().st_size

                # Check if file exceeds max size limit
                if max_file_bytes is not None and file_size > max_file_bytes:
                    node_content = f"<file too large: {file_size} bytes>"
                    logging.info(f"Skipping large file {entry.name}: {file_size} bytes > {max_file_bytes} bytes")
                # Check if file is binary
                elif _is_binary_file(entry):
                    node_content = f"<binary file: {file_size} bytes>"
                    logging.debug(f"Detected binary file {entry.name}")
                else:
                    # Try to read the file directly and handle all possible errors
                    node_content = entry.read_text(encoding="utf-8")
                    if isinstance(node_content, str):
                        cleaned_content = node_content.replace("\x00", "")
                        if cleaned_content != node_content:
                            logging.warning(f"Removed NULL bytes from content of {entry.name}")
                            node_content = cleaned_content
                        # Ensure content always ends with a newline for consistency with old behavior
                        if node_content and not node_content.endswith("\n"):
                            node_content = node_content + "\n"
            except PermissionError:
                logging.error(f"Could not read {entry.name}: Permission denied")
                node_content = "<unreadable content>\n"
            except UnicodeDecodeError:
                logging.warning(f"Cannot decode {entry.name} as UTF-8. Marking as unreadable.")
                node_content = "<unreadable content: not utf-8>\n"
            except IOError as e_read:
                logging.error(f"Could not read {entry.name}: {e_read}")
                node_content = "<unreadable content>\n"
            except Exception as e_other:
                logging.error(f"Unexpected error reading {entry.name}: {e_other}")
                node_content = "<unreadable content: unexpected error>\n"

            node["content"] = node_content if node_content is not None else ""

        return node

    except Exception as e:
        logging.error(f"Failed to create node for {entry.name}: {e}")
        return None
