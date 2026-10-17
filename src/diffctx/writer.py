from __future__ import annotations

import io
import json
import logging
import os
import re
import sys
import tempfile
from collections.abc import Callable
from pathlib import Path
from typing import Any, TextIO

from diffctx._diffctx import fit_artifact, get_language_for_file

logger = logging.getLogger(__name__)

# All Cc-category control chars (C0 0x00-0x1F, DEL 0x7F, C1 0x80-0x9F) except
# \t/\n, which are legal literal characters inside a YAML block scalar. Every
# other member of this set is either unrepresentable in a YAML stream or
# breaks the line-splitting logic in _write_yaml_block (e.g. \r).
_YAML_PROBLEMATIC_RE = re.compile(r"[\x00-\x08\x0b-\x1f\x7f-\x9f\u2028\u2029]")

# Fallback \xHH escape for every Cc control char; the named escapes below
# override the entries that have a friendlier YAML short form.
_YAML_CONTROL_HEX_ESCAPES = {chr(cp): f"\\x{cp:02x}" for cp in [*range(0x00, 0x20), 0x7F, *range(0x80, 0xA0)]}

_YAML_BASE_ESCAPE_MAP = {
    **_YAML_CONTROL_HEX_ESCAPES,
    "\\": "\\\\",
    '"': '\\"',
    "\n": "\\n",
    "\r": "\\r",
    "\x00": "\\0",
    "\x08": "\\b",
    "\x0c": "\\f",
    "\x85": "\\x85",
    "\u2028": "\\u2028",
    "\u2029": "\\u2029",
}
_YAML_CONTENT_ESCAPE_MAP = {**_YAML_BASE_ESCAPE_MAP, "\t": "\\t"}

_YAML_STRING_ESCAPE_PATTERN = re.compile("[" + re.escape("".join(_YAML_BASE_ESCAPE_MAP)) + "]")
_YAML_CONTENT_ESCAPE_PATTERN = re.compile("[" + re.escape("".join(_YAML_CONTENT_ESCAPE_MAP)) + "]")

_BACKTICK_RUN_PATTERN = re.compile(r"`+")

_MAX_MARKDOWN_HEADING_DEPTH = 5  # depth 0-5 → ## to ###### (6 levels), deeper uses list items

PLACEHOLDER_PATTERNS = [
    "<unreadable content>",
    "<unreadable content: not utf-8>",
]


def _escape_yaml_string(s: str) -> str:
    if not _YAML_STRING_ESCAPE_PATTERN.search(s):
        return s
    return _YAML_STRING_ESCAPE_PATTERN.sub(lambda m: _YAML_BASE_ESCAPE_MAP[m.group()], s)


def _escape_yaml_content(s: str) -> str:
    if not _YAML_CONTENT_ESCAPE_PATTERN.search(s):
        return s
    return _YAML_CONTENT_ESCAPE_PATTERN.sub(lambda m: _YAML_CONTENT_ESCAPE_MAP[m.group()], s)


def _has_problematic_chars(s: str) -> bool:
    return _YAML_PROBLEMATIC_RE.search(s) is not None


def _write_yaml_block(file: TextIO, key: str, content: str, base_indent: str) -> None:
    content_indent = base_indent + "  "
    if not content:
        file.write(f'{base_indent}{key}: ""\n')
    elif _has_problematic_chars(content) or not content.strip():
        file.write(f'{base_indent}{key}: "{_escape_yaml_content(content)}"\n')
    else:
        # Chomping must match the tail exactly: clip (|) normalizes any tail
        # to one newline, silently adding one to newline-less files and
        # eating trailing blank lines.
        trailing_newlines = len(content) - len(content.rstrip("\n"))
        if trailing_newlines == 0:
            chomping = "-"
            body_lines = content.split("\n")
        elif trailing_newlines == 1:
            chomping = ""
            body_lines = content[:-1].split("\n")
        else:
            chomping = "+"
            body_lines = content[:-1].split("\n")
        file.write(f"{base_indent}{key}: |2{chomping}\n")
        for line in body_lines:
            if line:
                file.write(f"{content_indent}{line}\n")
            else:
                file.write("\n")


def _write_yaml_content(file: TextIO, content: str, base_indent: str) -> None:
    _write_yaml_block(file, "content", content, base_indent)


def _write_yaml_node(file: TextIO, node: dict[str, Any], indent: str = "") -> None:
    name = _escape_yaml_string(str(node["name"]))
    file.write(f'{indent}- name: "{name}"\n')
    file.write(f"{indent}  type: {node['type']}\n")

    if node.get("truncated"):
        file.write(f"{indent}  truncated: true\n")

    if node.get("unreadable"):
        file.write(f'{indent}  unreadable: "{_escape_yaml_string(str(node["unreadable"]))}"\n')

    if node.get("redactions"):
        file.write(f"{indent}  redactions: {node['redactions']}\n")

    if "content" in node:
        _write_yaml_content(file, node["content"], indent + "  ")

    if node.get("children"):
        file.write(f"{indent}  children:\n")
        for child in node["children"]:
            _write_yaml_node(file, child, indent + "  ")


def write_tree_yaml(file: TextIO, tree: dict[str, Any]) -> None:
    name = _escape_yaml_string(str(tree["name"]))
    file.write(f'name: "{name}"\n')
    file.write(f"type: {tree['type']}\n")

    if tree.get("children"):
        file.write("children:\n")
        for child in tree["children"]:
            _write_yaml_node(file, child, "  ")
    elif "content" in tree:
        _write_yaml_content(file, tree["content"], "")


def write_tree_json(file: TextIO, tree: dict[str, Any]) -> None:
    json.dump(tree, file, ensure_ascii=False, indent=2)
    file.write("\n")


_TREE_BRANCH = "├── "
_TREE_LAST = "└── "
_TREE_PIPE = "│   "
_TREE_SPACE = "    "


def _write_tree_text_node(file: TextIO, node: dict[str, Any], prefix: str, connector: str) -> None:
    name = node.get("name", "")
    node_type = node.get("type", "")

    display_name = f"{name}/" if node_type == "directory" else name
    file.write(f"{prefix}{connector}{display_name}\n")

    is_last = connector == _TREE_LAST
    child_prefix = prefix + (_TREE_SPACE if is_last else _TREE_PIPE)

    if "content" in node:
        content = node["content"]
        content_prefix = child_prefix.replace(_TREE_PIPE, _TREE_SPACE)
        if not content:
            file.write(f"{content_prefix}(empty file)\n")
        else:
            for line in content.rstrip("\n").split("\n"):
                file.write(f"{content_prefix}{line}\n")

    if node.get("truncated"):
        content_prefix = child_prefix.replace(_TREE_PIPE, _TREE_SPACE)
        file.write(f"{content_prefix}(children omitted: depth limit)\n")
    if node.get("unreadable"):
        content_prefix = child_prefix.replace(_TREE_PIPE, _TREE_SPACE)
        file.write(f"{content_prefix}(unreadable directory: {node['unreadable']})\n")

    children = node.get("children", [])
    for i, child in enumerate(children):
        child_connector = _TREE_LAST if i == len(children) - 1 else _TREE_BRANCH
        _write_tree_text_node(file, child, child_prefix, child_connector)


def _write_tree_text_children(file: TextIO, children: list[dict[str, Any]]) -> None:
    for i, child in enumerate(children):
        connector = _TREE_LAST if i == len(children) - 1 else _TREE_BRANCH
        _write_tree_text_node(file, child, "", connector)


def _write_tree_text_content(file: TextIO, content: str) -> None:
    if not content:
        file.write("(empty file)\n")
    else:
        for line in content.rstrip("\n").split("\n"):
            file.write(f"{line}\n")


def write_tree_text(file: TextIO, tree: dict[str, Any]) -> None:
    name = tree.get("name", "")
    tree_type = tree.get("type", "")

    if tree_type == "file":
        file.write(f"{name}\n")
    else:
        file.write(f"{name}/\n")

    if tree.get("children"):
        _write_tree_text_children(file, tree["children"])
    elif "content" in tree:
        _write_tree_text_content(file, tree["content"])


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
        file.write(f"{content_indent}_(empty file)_\n\n")
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
        if node.get("truncated"):
            file.write(f"{content_indent}_(children omitted: --max-depth reached)_\n\n")
        elif node.get("unreadable"):
            file.write(f"{content_indent}_(unreadable directory: {node['unreadable']})_\n\n")
        else:
            file.write(f"{content_indent}_(empty directory)_\n\n")

    for child in node.get("children", []):
        _write_markdown_node(file, child, depth + 1)


def write_tree_markdown(file: TextIO, tree: dict[str, Any]) -> None:
    name = tree.get("name", "")
    tree_type = tree.get("type", "")
    if tree_type == "file":
        file.write(f"# {name}\n\n")
    else:
        file.write(f"# {name}/\n\n")

    if tree.get("children"):
        for child in tree["children"]:
            _write_markdown_node(file, child, 1)
    elif "content" in tree:
        _write_md_content(file, tree, name, "")


_WRITERS: dict[str, Callable[[TextIO, dict[str, Any]], None]] = {
    "json": write_tree_json,
    "txt": write_tree_text,
    "md": write_tree_markdown,
    "yaml": write_tree_yaml,
}


def tree_to_string(tree: dict[str, Any], output_format: str = "yaml") -> str:
    if tree.get("type") == "diff_context":
        # A diff artifact renders and fits to its budget in the engine, the
        # one fit every door shares (#507).
        return fit_artifact(tree, output_format)[1]
    buf = io.StringIO()
    _WRITERS.get(output_format, write_tree_yaml)(buf, tree)
    return buf.getvalue()


def _write_to_stdout_with_wrapper(writer: Callable[[TextIO], None]) -> bool:
    try:
        buf = sys.stdout.buffer
    except AttributeError:
        buf = None

    try:
        if buf:
            original_encoding = sys.stdout.encoding or sys.getdefaultencoding()
            original_errors = getattr(sys.stdout, "errors", "strict")
            utf8_stdout = io.TextIOWrapper(buf, encoding="utf-8", errors="backslashreplace", newline="")
            try:
                writer(utf8_stdout)
                utf8_stdout.flush()
            finally:
                utf8_stdout.detach()
                sys.stdout = io.TextIOWrapper(buf, encoding=original_encoding, errors=original_errors)
        else:
            writer(sys.stdout)
            sys.stdout.flush()
        return True
    except BrokenPipeError:
        return False


def _write_to_file_path(output_file: Path, writer: Callable[[TextIO], None]) -> None:
    output_file.parent.mkdir(parents=True, exist_ok=True)

    if output_file.is_dir():
        logger.error("Cannot write to '%s': is a directory", output_file)
        raise IsADirectoryError(f"Is a directory: {output_file}")

    try:
        fd_int, tmp_path = tempfile.mkstemp(dir=output_file.parent, suffix=".tmp")
    except PermissionError:
        logger.exception("Unable to write to file '%s': permission denied", output_file)
        raise
    except OSError:
        logger.exception("Unable to write to file '%s'", output_file)
        raise
    try:
        # mkstemp creates 0600; the artifact replacing the target must carry
        # the mode the caller's umask would give a new file, or `-o out.md`
        # silently turns a shared, world-readable output into owner-only.
        umask = os.umask(0)
        os.umask(umask)
        # newline="" keeps the file byte-identical to the stdout artifact; a
        # Windows text write would otherwise turn every LF into CRLF.
        with open(fd_int, "w", encoding="utf-8", errors="backslashreplace", newline="") as f:
            writer(f)
            f.flush()
            os.fsync(f.fileno())
            # 0o666 & ~umask is exactly what `open(..., "w")` would have
            # created; this never grants more than a plain write would. On the
            # descriptor, not the path: a chmod by name follows a symlink swapped
            # in under the temp name. Windows has no fchmod before 3.13 and no
            # mode bits to set, so it skips the call.
            if hasattr(os, "fchmod"):
                os.fchmod(f.fileno(), 0o666 & ~umask)
        os.replace(tmp_path, output_file)
    except PermissionError:
        Path(tmp_path).unlink(missing_ok=True)
        logger.exception("Unable to write to file '%s': permission denied", output_file)
        raise
    except OSError:
        Path(tmp_path).unlink(missing_ok=True)
        logger.exception("Unable to write to file '%s'", output_file)
        raise
    except BaseException:
        Path(tmp_path).unlink(missing_ok=True)
        raise


def write_string_to_file(content: str, output_file: Path | None, output_format: str = "yaml") -> None:
    def writer(f: TextIO) -> None:
        f.write(content)

    if output_file is None:
        if not _write_to_stdout_with_wrapper(writer):
            # The documented exit for `| head` is 141, and only the caller's
            # handler can produce it; swallowing the break here made tree and
            # pack mode exit 0 on a truncated artifact.
            raise BrokenPipeError
        logger.info("Output written to stdout in %s format", output_format)
    else:
        _write_to_file_path(output_file, writer)
        logger.info("Output saved to %s in %s format", output_file, output_format)
