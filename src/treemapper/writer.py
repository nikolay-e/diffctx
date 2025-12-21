import io
import json
import logging
import sys
from pathlib import Path
from typing import Any, Dict, Optional, TextIO

import yaml


class LiteralStr(str):
    pass


_yaml_representer_registered = False


def _ensure_yaml_representer() -> None:
    global _yaml_representer_registered
    if _yaml_representer_registered:
        return

    def representer(dumper: yaml.SafeDumper, data: LiteralStr) -> yaml.ScalarNode:
        style = "|" if data and not data.endswith("\n") else "|+"
        return dumper.represent_scalar("tag:yaml.org,2002:str", data, style=style)

    yaml.add_representer(LiteralStr, representer, Dumper=yaml.SafeDumper)
    _yaml_representer_registered = True


def _prepare_tree_for_yaml(node: Dict[str, Any]) -> Dict[str, Any]:
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
    _ensure_yaml_representer()
    prepared = _prepare_tree_for_yaml(tree)
    yaml.safe_dump(prepared, file, allow_unicode=True, sort_keys=False, default_flow_style=False)


def write_tree_json(file: TextIO, tree: Dict[str, Any]) -> None:
    json.dump(tree, file, ensure_ascii=False, indent=2)
    file.write("\n")


def _write_tree_text_node(file: TextIO, node: Dict[str, Any], prefix: str, is_last: bool) -> None:
    connector = "└── " if is_last else "├── "
    name = node.get("name", "")
    node_type = node.get("type", "")

    if node_type == "directory":
        file.write(f"{prefix}{connector}{name}/\n")
    else:
        file.write(f"{prefix}{connector}{name}\n")

    child_prefix = prefix + ("    " if is_last else "│   ")

    if "content" in node:
        content = node["content"]
        for line in content.splitlines():
            file.write(f"{child_prefix}  {line}\n")

    children = node.get("children", [])
    for i, child in enumerate(children):
        _write_tree_text_node(file, child, child_prefix, i == len(children) - 1)


def write_tree_text(file: TextIO, tree: Dict[str, Any]) -> None:
    name = tree.get("name", "")
    file.write(f"{name}/\n")

    children = tree.get("children", [])
    for i, child in enumerate(children):
        _write_tree_text_node(file, child, "", i == len(children) - 1)


def write_tree_to_file(tree: Dict[str, Any], output_file: Optional[Path], output_format: str = "yaml") -> None:
    def write_tree_content(f: TextIO) -> None:
        if output_format == "json":
            write_tree_json(f, tree)
        elif output_format == "text":
            write_tree_text(f, tree)
        else:  # yaml
            write_tree_yaml(f, tree)

    if output_file is None:
        try:
            buf = sys.stdout.buffer
        except AttributeError:
            buf = None

        if buf:
            utf8_stdout = io.TextIOWrapper(buf, encoding="utf-8", newline="")
            try:
                write_tree_content(utf8_stdout)
                utf8_stdout.flush()
            finally:
                utf8_stdout.detach()
        else:
            write_tree_content(sys.stdout)
            sys.stdout.flush()

        logging.info(f"Directory tree written to stdout in {output_format} format")
    else:
        try:
            output_file.parent.mkdir(parents=True, exist_ok=True)

            if output_file.is_dir():
                logging.error(f"Cannot write to '{output_file}': is a directory")
                raise IOError(f"Is a directory: {output_file}")

            with output_file.open("w", encoding="utf-8") as f:
                write_tree_content(f)
            logging.info(f"Directory tree saved to {output_file} in {output_format} format")
        except PermissionError as e:
            logging.error(f"Unable to write to file '{output_file}': Permission denied")
            raise IOError(f"Permission denied: {output_file}") from e
        except IOError as e:
            logging.error(f"Unable to write to file '{output_file}': {e}")
            raise
