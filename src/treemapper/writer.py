import logging
from pathlib import Path
from typing import Any, Dict


def write_yaml_node(file, node: Dict[str, Any], indent: str = "") -> None:
    """Write a node of the directory tree in YAML format."""
    file.write(f"{indent}- name: {node['name']}\n")
    file.write(f"{indent}  type: {node['type']}\n")

    if "content" in node:
        file.write(f"{indent}  content: |\n")
        for line in node["content"].splitlines():
            file.write(f"{indent}    {line}\n")

    if "children" in node and node["children"]:
        file.write(f"{indent}  children:\n")
        for child in node["children"]:
            write_yaml_node(file, child, indent + "  ")


def write_tree_to_file(tree: Dict[str, Any], output_file: Path) -> None:
    """Write the complete tree to a YAML file."""
    try:

        output_file.parent.mkdir(parents=True, exist_ok=True)

        with output_file.open("w", encoding="utf-8") as f:
            f.write(f"name: {tree['name']}\n")
            f.write(f"type: {tree['type']}\n")
            if "children" in tree and tree["children"]:
                f.write("children:\n")
                for child in tree["children"]:
                    write_yaml_node(f, child, "  ")
        logging.info(f"Directory tree saved to {output_file}")
    except IOError as e:
        logging.error(f"Unable to write to file '{output_file}': {e}")
        raise
