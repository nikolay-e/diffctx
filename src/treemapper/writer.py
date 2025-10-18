import io
import json
import logging
import sys
from pathlib import Path
from typing import Any, Dict, Optional, TextIO

import yaml


class LiteralStr(str):
    """String subclass to force PyYAML to use literal (|) block style for multi-line content."""

    pass


def _literal_str_representer(dumper: yaml.SafeDumper, data: LiteralStr) -> yaml.ScalarNode:
    """PyYAML representer to emit LiteralStr as literal block style with keep semantics."""
    # Use |+ to keep all trailing newlines (including the final one)
    # This ensures exact preservation of file content
    if data and not data.endswith("\n"):
        # If there's no trailing newline, ensure clip behavior still works correctly
        return dumper.represent_scalar("tag:yaml.org,2002:str", data, style="|")
    return dumper.represent_scalar("tag:yaml.org,2002:str", data, style="|+")


# Register the representer for LiteralStr
yaml.add_representer(LiteralStr, _literal_str_representer, Dumper=yaml.SafeDumper)


def _prepare_tree_for_yaml(node: Dict[str, Any]) -> Dict[str, Any]:
    """
    Recursively prepare the tree for YAML output by wrapping multi-line content as LiteralStr.
    This ensures proper block-style formatting in YAML.
    """
    result: Dict[str, Any] = {}
    for key, value in node.items():
        if key == "content" and isinstance(value, str) and "\n" in value:
            result[key] = LiteralStr(value)
        elif key == "children" and isinstance(value, list):
            result[key] = [_prepare_tree_for_yaml(child) for child in value]
        else:
            result[key] = value
    return result


def write_tree_yaml(file: TextIO, tree: Dict[str, Any]) -> None:
    """Write the tree in YAML format using PyYAML."""
    prepared = _prepare_tree_for_yaml(tree)
    yaml.safe_dump(prepared, file, allow_unicode=True, sort_keys=False, default_flow_style=False)


def write_tree_json(file: TextIO, tree: Dict[str, Any]) -> None:
    """Write the tree in JSON format."""
    json.dump(tree, file, ensure_ascii=False, indent=2)
    file.write("\n")  # Add trailing newline for consistency


def _write_tree_text_node(file: TextIO, node: Dict[str, Any], indent: str = "") -> None:
    """Write a single node in plain text format."""
    name = node.get("name", "")
    node_type = node.get("type", "")

    if node_type == "directory":
        file.write(f"{indent}{name}/\n")
    else:
        file.write(f"{indent}{name}\n")

    if "content" in node:
        content = node["content"]
        file.write(f"{indent}  --- BEGIN CONTENT ---\n")
        for line in content.splitlines():
            file.write(f"{indent}  {line}\n")
        file.write(f"{indent}  --- END CONTENT ---\n")

    if "children" in node and node["children"]:
        for child in node["children"]:
            _write_tree_text_node(file, child, indent + "  ")


def write_tree_text(file: TextIO, tree: Dict[str, Any]) -> None:
    """Write the tree in plain text format with clear delimiters."""
    name = tree.get("name", "")
    file.write(f"Directory Tree: {name}\n")
    file.write("=" * 80 + "\n\n")

    if "children" in tree and tree["children"]:
        for child in tree["children"]:
            _write_tree_text_node(file, child, "")

    file.write("\n" + "=" * 80 + "\n")


def write_tree_to_file(tree: Dict[str, Any], output_file: Optional[Path], output_format: str = "yaml") -> None:
    """Write the complete tree to a file or stdout in the specified format."""

    def write_tree_content(f: TextIO) -> None:
        """Write the tree content to the given file handle."""
        if output_format == "json":
            write_tree_json(f, tree)
        elif output_format == "text":
            write_tree_text(f, tree)
        else:  # yaml
            write_tree_yaml(f, tree)

    if output_file is None:
        # Write to stdout with proper UTF-8 encoding
        # On Windows, sys.stdout might use cp1252 encoding which can't handle Unicode
        try:
            buf = sys.stdout.buffer
        except AttributeError:
            # Some environments (e.g., certain IDEs, tests with StringIO) don't have buffer
            buf = None

        if buf:
            # Use TextIOWrapper to ensure UTF-8 encoding for stdout
            utf8_stdout = io.TextIOWrapper(buf, encoding="utf-8", newline="")
            write_tree_content(utf8_stdout)
            utf8_stdout.flush()
        else:
            # Fallback: write directly to sys.stdout
            write_tree_content(sys.stdout)
            sys.stdout.flush()

        logging.info(f"Directory tree written to stdout in {output_format} format")
    else:
        # Write to file
        try:
            # Create parent directories if they don't exist
            output_file.parent.mkdir(parents=True, exist_ok=True)

            # For directories, check early
            if output_file.is_dir():
                logging.error(f"Unable to write to file '{output_file}': Is a directory")
                raise IOError(f"Is a directory: {output_file}")

            # Try to write the file directly; exceptions will be caught
            with output_file.open("w", encoding="utf-8") as f:
                write_tree_content(f)
            logging.info(f"Directory tree saved to {output_file} in {output_format} format")
        except PermissionError as e:
            logging.error(f"Unable to write to file '{output_file}': Permission denied")
            raise IOError(f"Permission denied: {output_file}") from e
        except IOError as e:
            logging.error(f"Unable to write to file '{output_file}': {e}")
            raise
