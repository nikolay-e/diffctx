from __future__ import annotations

import io
import json
import logging
import re
import sys
from collections.abc import Callable
from pathlib import Path
from typing import Any, TextIO

from treemapper.diffctx.languages import (
    EXTENSION_TO_LANGUAGE,
    FILENAME_TO_LANGUAGE,
    get_language_for_file,
)

YAML_PROBLEMATIC_CHARS = frozenset({"\x85", "\u2028", "\u2029"})

_YAML_STRING_ESCAPE_PATTERN = re.compile(r'[\\"\n\r\x00\x85\u2028\u2029]')
_YAML_STRING_ESCAPE_MAP = {
    "\\": "\\\\",
    '"': '\\"',
    "\n": "\\n",
    "\r": "\\r",
    "\x00": "\\0",
    "\x85": "\\x85",
    "\u2028": "\\u2028",
    "\u2029": "\\u2029",
}

_YAML_CONTENT_ESCAPE_PATTERN = re.compile(r'[\\"\n\t\r\x00\x85\u2028\u2029]')
_YAML_CONTENT_ESCAPE_MAP = {
    "\\": "\\\\",
    '"': '\\"',
    "\n": "\\n",
    "\t": "\\t",
    "\r": "\\r",
    "\x00": "\\0",
    "\x85": "\\x85",
    "\u2028": "\\u2028",
    "\u2029": "\\u2029",
}

_BACKTICK_RUN_PATTERN = re.compile(r"`+")

_MAX_MARKDOWN_HEADING_DEPTH = 5  # depth 0-5 → ## to ###### (6 levels), deeper uses list items

EXTENSION_TO_LANG = EXTENSION_TO_LANGUAGE
FILENAME_TO_LANG = FILENAME_TO_LANGUAGE

PLACEHOLDER_PATTERNS = [
    "<unreadable content>",
    "<unreadable content: not utf-8>",
]


def _escape_yaml_string(s: str) -> str:
    if not _YAML_STRING_ESCAPE_PATTERN.search(s):
        return s
    return _YAML_STRING_ESCAPE_PATTERN.sub(lambda m: _YAML_STRING_ESCAPE_MAP[m.group()], s)


def _escape_yaml_content(s: str) -> str:
    if not _YAML_CONTENT_ESCAPE_PATTERN.search(s):
        return s
    return _YAML_CONTENT_ESCAPE_PATTERN.sub(lambda m: _YAML_CONTENT_ESCAPE_MAP[m.group()], s)


def _has_problematic_chars(s: str) -> bool:
    return any(c in s for c in YAML_PROBLEMATIC_CHARS)


def _write_yaml_content(file: TextIO, content: str, base_indent: str) -> None:
    content_indent = base_indent + "  "
    if not content:
        file.write(f'{base_indent}content: ""\n')
    elif _has_problematic_chars(content):
        file.write(f'{base_indent}content: "{_escape_yaml_content(content)}"\n')
    else:
        file.write(f"{base_indent}content: |\n")
        for line in content.rstrip("\n").split("\n"):
            file.write(f"{content_indent}{line}\n")


def _write_yaml_node(file: TextIO, node: dict[str, Any], indent: str = "") -> None:
    name = _escape_yaml_string(str(node["name"]))
    file.write(f'{indent}- name: "{name}"\n')
    file.write(f"{indent}  type: {node['type']}\n")

    if "content" in node:
        _write_yaml_content(file, node["content"], indent + "  ")

    if node.get("children"):
        file.write(f"{indent}  children:\n")
        for child in node["children"]:
            _write_yaml_node(file, child, indent + "  ")


def _write_yaml_fragment(file: TextIO, frag: dict[str, Any], indent: str = "") -> None:
    file.write(f"{indent}- path: \"{_escape_yaml_string(frag.get('path', ''))}\"\n")
    file.write(f"{indent}  lines: \"{frag.get('lines', '')}\"\n")
    file.write(f"{indent}  kind: {frag.get('kind', 'unknown')}\n")

    if frag.get("symbol"):
        file.write(f"{indent}  symbol: \"{_escape_yaml_string(frag['symbol'])}\"\n")

    if "content" in frag:
        _write_yaml_content(file, frag["content"], indent + "  ")


def write_tree_yaml(file: TextIO, tree: dict[str, Any]) -> None:
    name = _escape_yaml_string(str(tree["name"]))
    file.write(f'name: "{name}"\n')
    file.write(f"type: {tree['type']}\n")

    if tree.get("type") == "diff_context" and tree.get("fragments"):
        file.write("fragments:\n")
        for frag in tree["fragments"]:
            _write_yaml_fragment(file, frag, "  ")
    elif tree.get("children"):
        file.write("children:\n")
        for child in tree["children"]:
            _write_yaml_node(file, child, "  ")


def write_tree_json(file: TextIO, tree: dict[str, Any]) -> None:
    json.dump(tree, file, ensure_ascii=False, indent=2)
    file.write("\n")


def _write_tree_text_node(file: TextIO, node: dict[str, Any], indent: str = "") -> None:
    name = node.get("name", "")
    node_type = node.get("type", "")

    if node_type == "directory":
        file.write(f"{indent}{name}/\n")
    else:
        file.write(f"{indent}{name}\n")

    if node.get("content"):
        content = node["content"]
        content_indent = indent + "  "
        for line in content.rstrip("\n").split("\n"):
            file.write(f"{content_indent}{line}\n")

    if node.get("children"):
        for child in node["children"]:
            _write_tree_text_node(file, child, indent + "  ")


def _write_text_fragment(file: TextIO, frag: dict[str, Any], indent: str = "") -> None:
    path = frag.get("path", "")
    lines = frag.get("lines", "")
    kind = frag.get("kind", "")
    symbol = frag.get("symbol", "")

    header = f"{path}:{lines}"
    if symbol:
        header += f" ({symbol})"
    if kind:
        header += f" [{kind}]"
    file.write(f"{indent}{header}\n")

    if frag.get("content"):
        content = frag["content"]
        content_indent = indent + "  "
        for line in content.rstrip("\n").split("\n"):
            file.write(f"{content_indent}{line}\n")


def write_tree_text(file: TextIO, tree: dict[str, Any]) -> None:
    name = tree.get("name", "")
    file.write(f"{name}/\n")

    if tree.get("type") == "diff_context" and tree.get("fragments"):
        for frag in tree["fragments"]:
            _write_text_fragment(file, frag, "  ")
    elif tree.get("children"):
        for child in tree["children"]:
            _write_tree_text_node(file, child, "  ")


def _is_placeholder(content: str) -> bool:
    stripped = content.strip()
    if stripped in PLACEHOLDER_PATTERNS:
        return True
    if stripped.startswith("<binary file:") and stripped.endswith(">"):
        return True
    if stripped.startswith("<file too large:") and stripped.endswith(">"):
        return True
    return False


def _infer_language(filename: str) -> str:
    return get_language_for_file(filename) or ""


def _get_fence_length(content: str) -> int:
    matches = _BACKTICK_RUN_PATTERN.findall(content)
    if not matches:
        return 3
    return max(3, max(len(m) for m in matches) + 1)


def _write_md_header(file: TextIO, display_name: str, depth: int, list_indent: str) -> None:
    if depth <= _MAX_MARKDOWN_HEADING_DEPTH:
        heading = "#" * (depth + 1)
        file.write(f"{heading} {display_name}\n\n")
    else:
        file.write(f"{list_indent}- **{display_name}**\n\n")


def _write_md_code_block(file: TextIO, content: str, lang: str, indent: str) -> None:
    fence_len = _get_fence_length(content)
    fence = "`" * fence_len
    file.write(f"{indent}{fence}{lang}\n")
    for line in content.splitlines(keepends=True):
        file.write(f"{indent}{line}")
    if not content.endswith("\n"):
        file.write("\n")
    file.write(f"{indent}{fence}\n\n")


def _write_md_content(file: TextIO, node: dict[str, Any], name: str, content_indent: str) -> None:
    content = node["content"]
    if not content:
        return
    if _is_placeholder(content):
        file.write(f"{content_indent}_{content.strip()}_\n\n")
    else:
        lang = _infer_language(name)
        _write_md_code_block(file, content, lang, content_indent)


def _write_markdown_node(file: TextIO, node: dict[str, Any], depth: int) -> None:
    name = node.get("name", "")
    is_dir = node.get("type", "") == "directory"
    display_name = f"{name}/" if is_dir else name

    in_list = depth > _MAX_MARKDOWN_HEADING_DEPTH
    list_indent = "  " * (depth - _MAX_MARKDOWN_HEADING_DEPTH) if in_list else ""
    content_indent = list_indent + "  " if in_list else ""

    _write_md_header(file, display_name, depth, list_indent)

    if "content" in node:
        _write_md_content(file, node, name, content_indent)
    elif is_dir and not node.get("children"):
        file.write(f"{content_indent}_(empty directory)_\n\n")

    for child in node.get("children", []):
        _write_markdown_node(file, child, depth + 1)


def _write_markdown_fragment(file: TextIO, frag: dict[str, Any]) -> None:
    path = frag.get("path", "")
    lines = frag.get("lines", "")
    kind = frag.get("kind", "")
    symbol = frag.get("symbol", "")

    header = f"`{path}:{lines}`"
    if symbol:
        header += f" **{symbol}**"
    if kind:
        header += f" _{kind}_"
    file.write(f"## {header}\n\n")

    if frag.get("content"):
        lang = _infer_language(path.split("/")[-1] if "/" in path else path)
        _write_md_code_block(file, frag["content"], lang, "")


def write_tree_markdown(file: TextIO, tree: dict[str, Any]) -> None:
    name = tree.get("name", "")
    file.write(f"# {name}/\n\n")

    if tree.get("type") == "diff_context" and tree.get("fragments"):
        for frag in tree["fragments"]:
            _write_markdown_fragment(file, frag)
    elif tree.get("children"):
        for child in tree["children"]:
            _write_markdown_node(file, child, 1)


def tree_to_string(tree: dict[str, Any], output_format: str = "yaml") -> str:
    buf = io.StringIO()
    if output_format == "json":
        write_tree_json(buf, tree)
    elif output_format == "txt":
        write_tree_text(buf, tree)
    elif output_format == "md":
        write_tree_markdown(buf, tree)
    else:
        write_tree_yaml(buf, tree)
    return buf.getvalue()


def _write_to_stdout_with_wrapper(writer: Callable[[TextIO], None]) -> None:
    try:
        buf = sys.stdout.buffer
    except AttributeError:
        buf = None

    try:
        if buf:
            utf8_stdout = io.TextIOWrapper(buf, encoding="utf-8", newline="")
            try:
                writer(utf8_stdout)
                utf8_stdout.flush()
            finally:
                utf8_stdout.detach()
        else:
            writer(sys.stdout)
            sys.stdout.flush()
    except BrokenPipeError:
        pass


def _write_to_file_path(output_file: Path, writer: Callable[[TextIO], None]) -> None:
    output_file.parent.mkdir(parents=True, exist_ok=True)

    if output_file.is_dir():
        logging.error("Cannot write to '%s': is a directory", output_file)
        raise IsADirectoryError(f"Is a directory: {output_file}")

    try:
        with output_file.open("w", encoding="utf-8") as f:
            writer(f)
    except PermissionError:
        logging.error("Unable to write to file '%s': Permission denied", output_file)
        raise
    except OSError as e:
        logging.error("Unable to write to file '%s': %s", output_file, e)
        raise


def write_string_to_file(content: str, output_file: Path | None, output_format: str = "yaml") -> None:
    def writer(f: TextIO) -> None:
        f.write(content)

    if output_file is None:
        _write_to_stdout_with_wrapper(writer)
        logging.info("Directory tree written to stdout in %s format", output_format)
    else:
        _write_to_file_path(output_file, writer)
        logging.info("Directory tree saved to %s in %s format", output_file, output_format)


def write_tree_to_file(tree: dict[str, Any], output_file: Path | None, output_format: str = "yaml") -> None:
    def writer(f: TextIO) -> None:
        if output_format == "json":
            write_tree_json(f, tree)
        elif output_format == "txt":
            write_tree_text(f, tree)
        elif output_format == "md":
            write_tree_markdown(f, tree)
        else:
            write_tree_yaml(f, tree)

    if output_file is None:
        _write_to_stdout_with_wrapper(writer)
        logging.info("Directory tree written to stdout in %s format", output_format)
    else:
        _write_to_file_path(output_file, writer)
        logging.info("Directory tree saved to %s in %s format", output_file, output_format)
