# src/treemapper/tree.py
import logging
from pathlib import Path
from typing import Any, Dict, List, Optional

from pathspec import pathspec

from .ignore import should_ignore


def build_tree(
    dir_path: Path, base_dir: Path, combined_spec: pathspec.PathSpec, gitignore_specs: Dict[Path, pathspec.PathSpec]
) -> List[Dict[str, Any]]:
    """Build the directory tree structure."""
    tree = []
    try:
        for entry in sorted(dir_path.iterdir()):
            try:
                relative_path = entry.relative_to(base_dir).as_posix()
                is_dir_entry = entry.is_dir()
            except OSError as e:
                logging.warning(f"Could not process path for entry {entry}: {e}")
                continue

            if is_dir_entry:
                relative_path_check = relative_path + "/"
            else:
                relative_path_check = relative_path

            if should_ignore(relative_path_check, combined_spec):
                continue

            if should_ignore_git(entry, relative_path_check, gitignore_specs, base_dir):
                continue

            if not entry.exists() or entry.is_symlink():
                logging.debug(f"Skipping '{relative_path_check}': not exists or is symlink")
                continue

            node = create_node(entry, base_dir, combined_spec, gitignore_specs)
            if node:
                tree.append(node)

    except PermissionError:
        logging.warning(f"Permission denied accessing directory {dir_path}")
    except OSError as e:
        logging.warning(f"Error accessing directory {dir_path}: {e}")

    return tree


def should_ignore_git(
    entry: Path, relative_path_check: str, gitignore_specs: Dict[Path, pathspec.PathSpec], base_dir: Path
) -> bool:
    """Check if entry should be ignored based on applicable gitignore specs."""
    if not gitignore_specs:
        return False

    for git_dir_path, git_spec in gitignore_specs.items():
        try:
            if entry == git_dir_path or entry.is_relative_to(git_dir_path):
                rel_path_to_git_dir = entry.relative_to(git_dir_path).as_posix()
                if entry.is_dir() and not rel_path_to_git_dir.endswith("/"):
                    rel_path_to_git_dir += "/"

                logging.debug(
                    f"Checking path '{rel_path_to_git_dir}' against spec from '{git_dir_path}' with patterns: {git_spec.patterns}"
                )

                if git_spec.match_file(rel_path_to_git_dir):
                    try:
                        gitignore_location = git_dir_path.relative_to(base_dir).as_posix()
                        if not gitignore_location:
                            gitignore_location = "."
                    except ValueError:
                        gitignore_location = str(git_dir_path)
                    logging.debug(f"Ignoring '{relative_path_check}' based on .gitignore in '{gitignore_location}'")
                    return True
        except ValueError:
            continue
        except Exception as e:
            logging.warning(f"Error checking gitignore spec from {git_dir_path} against {entry}: {e}")
            continue

    return False


def create_node(
    entry: Path, base_dir: Path, combined_spec: pathspec.PathSpec, gitignore_specs: Dict[Path, pathspec.PathSpec]
) -> Optional[Dict[str, Any]]:
    """Create a node for the tree structure. Returns None if node creation fails."""
    try:
        node_type = "directory" if entry.is_dir() else "file"

        node: Dict[str, Any] = {"name": entry.name, "type": node_type}

        if node_type == "directory":
            children = build_tree(entry, base_dir, combined_spec, gitignore_specs)
            if children:
                node["children"] = children
        elif node_type == "file":

            node_content: Optional[str] = None
            try:
                node_content = entry.read_text(encoding="utf-8")
                if isinstance(node_content, str):
                    cleaned_content = node_content.replace("\x00", "")
                    if cleaned_content != node_content:
                        logging.warning(f"Removed NULL bytes from content of {entry.name}")
                        node_content = cleaned_content
            except UnicodeDecodeError:
                logging.warning(f"Cannot decode {entry.name} as UTF-8. Marking as unreadable.")
                node_content = "<unreadable content: not utf-8>"
            except IOError as e_read:
                logging.error(f"Could not read {entry.name}: {e_read}")
                node_content = "<unreadable content>"
            except Exception as e_other:
                logging.error(f"Unexpected error reading {entry.name}: {e_other}")
                node_content = "<unreadable content: unexpected error>"

            node["content"] = node_content if node_content is not None else ""

        return node

    except Exception as e:
        logging.error(f"Failed to create node for {entry.name}: {e}")
        return None
