from __future__ import annotations

import io
import json
import logging
import os
import re
import sys
import tempfile
from collections.abc import Callable
from pathlib import Path, PurePosixPath
from typing import Any, TextIO

from diffctx._diffctx import count_tokens, get_language_for_file

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
_MD_SPECIAL_CHARS = re.compile(r"([\\`*_\[\]()#>+\-~|{}!])")

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


def _escape_markdown(s: str) -> str:
    return _MD_SPECIAL_CHARS.sub(r"\\\1", s)


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


def _write_yaml_fragment(file: TextIO, frag: dict[str, Any], indent: str = "") -> None:
    file.write(f'{indent}- path: "{_escape_yaml_string(frag.get("path", ""))}"\n')
    file.write(f'{indent}  lines: "{_escape_yaml_string(frag.get("lines", ""))}"\n')
    if frag.get("role"):
        file.write(f'{indent}  role: "{_escape_yaml_string(frag["role"])}"\n')
    file.write(f'{indent}  kind: "{_escape_yaml_string(frag.get("kind", "unknown"))}"\n')

    if frag.get("symbol"):
        file.write(f'{indent}  symbol: "{_escape_yaml_string(frag["symbol"])}"\n')

    if "content" in frag:
        _write_yaml_content(file, frag["content"], indent + "  ")


def _has_diff_metadata(tree: dict[str, Any]) -> bool:
    # changed_files counts: a selection that came back empty still has to say
    # which files the range touched, or the only actionable fact about the run
    # is dropped and the reader is left with a name/type stub.
    return bool(
        tree.get("fragments")
        or tree.get("deleted_files")
        or tree.get("renamed_files")
        or tree.get("changed_files")
        or tree.get("commit_message")
        or tree.get("lockfile_changes")
        or tree.get("ignored_changes")
        or tree.get("policy_excluded_count")
        or tree.get("raw_diff")
    )


def _write_yaml_path_list(file: TextIO, key: str, paths: list[Any]) -> None:
    file.write(f"{key}:\n")
    for path in paths:
        file.write(f'  - "{_escape_yaml_string(str(path))}"\n')


def _write_yaml_list_item(file: TextIO, item: Any, indent: str) -> None:
    if not isinstance(item, dict):
        file.write(f'{indent}  - "{_escape_yaml_string(str(item))}"\n')
        return
    first = True
    for sub_key, sub_value in item.items():
        prefix = f"{indent}  - " if first else f"{indent}    "
        first = False
        buf = io.StringIO()
        _write_yaml_value(buf, str(sub_key), sub_value, "")
        file.write(prefix + buf.getvalue().replace("\n", f"\n{indent}    ").rstrip(" ").rstrip("\n") + "\n")


def _yaml_scalar(value: Any) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    if value is None:
        return "null"
    return f'"{_escape_yaml_string(str(value))}"'


def _write_yaml_value(file: TextIO, key: str, value: Any, indent: str) -> None:
    if isinstance(value, dict):
        if not value:
            file.write(f"{indent}{key}: {{}}\n")
            return
        file.write(f"{indent}{key}:\n")
        for sub_key, sub_value in value.items():
            _write_yaml_value(file, str(sub_key), sub_value, indent + "  ")
    elif isinstance(value, list):
        if not value:
            file.write(f"{indent}{key}: []\n")
            return
        file.write(f"{indent}{key}:\n")
        for item in value:
            _write_yaml_list_item(file, item, indent)
    else:
        file.write(f"{indent}{key}: {_yaml_scalar(value)}\n")


def _write_yaml_change_lists(file: TextIO, tree: dict[str, Any]) -> None:
    if tree.get("commit_message"):
        file.write(f'commit_message: "{_escape_yaml_string(str(tree["commit_message"]))}"\n')
    if tree.get("commit_messages"):
        _write_yaml_value(file, "commit_messages", tree["commit_messages"], "")
    if tree.get("changed_files"):
        _write_yaml_path_list(file, "changed_files", tree["changed_files"])
    if tree.get("changes"):
        _write_yaml_value(file, "changes", tree["changes"], "")
    if tree.get("deleted_files"):
        _write_yaml_path_list(file, "deleted_files", tree["deleted_files"])
    if tree.get("renamed_files"):
        file.write("renamed_files:\n")
        for pair in tree["renamed_files"]:
            file.write(f'  - from: "{_escape_yaml_string(str(pair.get("from", "")))}"\n')
            file.write(f'    to: "{_escape_yaml_string(str(pair.get("to", "")))}"\n')
    if tree.get("lockfile_changes"):
        _write_yaml_path_list(file, "lockfile_changes", tree["lockfile_changes"])
    if tree.get("ignored_changes"):
        _write_yaml_path_list(file, "ignored_changes", tree["ignored_changes"])
    if tree.get("policy_excluded_count"):
        file.write(f"policy_excluded_count: {tree['policy_excluded_count']}\n")


def _write_yaml_diff_metadata(file: TextIO, tree: dict[str, Any]) -> None:
    _write_yaml_change_lists(file, tree)
    if tree.get("raw_diff"):
        _write_yaml_block(file, "raw_diff", tree["raw_diff"], "")
    if tree.get("fragments"):
        file.write(f"fragment_count: {len(tree['fragments'])}\n")
        file.write("fragments:\n")
        for frag in tree["fragments"]:
            _write_yaml_fragment(file, frag, "  ")
    if tree.get("coverage"):
        _write_yaml_value(file, "coverage", tree["coverage"], "")
    if tree.get("provenance"):
        _write_yaml_value(file, "provenance", tree["provenance"], "")


def write_tree_yaml(file: TextIO, tree: dict[str, Any]) -> None:
    if tree.get("schema"):
        file.write(f"schema: {tree['schema']}\n")
    name = _escape_yaml_string(str(tree["name"]))
    file.write(f'name: "{name}"\n')
    file.write(f"type: {tree['type']}\n")

    if tree.get("type") == "diff_context" and _has_diff_metadata(tree):
        _write_yaml_diff_metadata(file, tree)
    elif tree.get("children"):
        file.write("children:\n")
        for child in tree["children"]:
            _write_yaml_node(file, child, "  ")
    elif "content" in tree:
        _write_yaml_content(file, tree["content"], "")


def write_tree_json(file: TextIO, tree: dict[str, Any]) -> None:
    json.dump({key: value for key, value in tree.items() if key != "latency"}, file, ensure_ascii=False, indent=2)
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
    if frag.get("role"):
        header += f" <{frag['role']}>"
    file.write(f"{indent}{header}\n")

    if frag.get("content"):
        content = frag["content"]
        content_indent = indent + "  "
        for line in content.rstrip("\n").split("\n"):
            file.write(f"{content_indent}{line}\n")


_OMITTED_TEXT_MARK = " (omitted)"
_NO_FRAGMENTS_TEXT_MARK = " (no fragments)"


def _escape_text_path(path: Any) -> str:
    # Backslash first, so an escaped marker below cannot be mistaken for a real
    # backslash the path carried; then the two line breaks git can emit
    # unquoted; then the suffixes the changed-files list itself appends — a
    # file literally named `report (omitted)` must not read as an omitted
    # `report`.
    text = str(path).replace("\\", "\\\\").replace("\n", "\\n").replace("\r", "\\r")
    for mark in (_OMITTED_TEXT_MARK, _NO_FRAGMENTS_TEXT_MARK):
        if text.endswith(mark):
            text = text[: -len(mark)] + " \\" + mark[1:]
    return text


def _write_text_path_list(file: TextIO, label: str, paths: list[Any]) -> None:
    # One path per line: a comma or newline inside a path (git emits both
    # unquoted under core.quotePath=false) would make a joined line unparseable.
    file.write(f"  {label}:\n")
    for path in paths:
        file.write(f"    {_escape_text_path(path)}\n")


def _write_text_raw_diff(file: TextIO, tree: dict[str, Any]) -> None:
    if not tree.get("raw_diff"):
        return
    file.write("  raw diff:\n")
    for line in tree["raw_diff"].rstrip("\n").split("\n"):
        file.write(f"    {line}\n" if line else "\n")


def _write_text_changed_files(file: TextIO, tree: dict[str, Any]) -> None:
    omitted = set(_omitted_changed_files(tree))
    fragmentless = set(_no_fragment_changed_files(tree))
    file.write("  changed files:\n")
    for path in tree["changed_files"]:
        text = str(path)
        mark = _NO_FRAGMENTS_TEXT_MARK if text in fragmentless else _OMITTED_TEXT_MARK if text in omitted else ""
        file.write(f"    {_escape_text_path(path)}{mark}\n")


def _commit_heading(tree: dict[str, Any], listed: int) -> str:
    total = tree.get("commit_count")
    if isinstance(total, int) and total > listed:
        return f"newest {listed} of {total} commits"
    return f"{listed} commits"


def _write_text_commit_messages(file: TextIO, tree: dict[str, Any]) -> None:
    if tree.get("commit_messages"):
        listed = len(tree["commit_messages"])
        total = tree.get("commit_count")
        shown = f"newest {listed} of {total}" if isinstance(total, int) and total > listed else str(listed)
        file.write(f"  commits: {shown}\n")
        for message in tree["commit_messages"]:
            for i, line in enumerate(str(message).splitlines()):
                file.write(f"    {'- ' if i == 0 else '  '}{line}\n")
    elif tree.get("commit_message"):
        file.write(f"  change: {tree['commit_message']}\n")


def _write_tree_text_diff_context(file: TextIO, tree: dict[str, Any]) -> None:
    _write_text_commit_messages(file, tree)
    if tree.get("changed_files"):
        _write_text_changed_files(file, tree)
    if tree.get("deleted_files"):
        _write_text_path_list(file, "deleted files", tree["deleted_files"])
    for pair in tree.get("renamed_files", []):
        file.write(f"  renamed: {pair.get('from', '')} -> {pair.get('to', '')}\n")
    if tree.get("lockfile_changes"):
        _write_text_path_list(file, "lock files changed", tree["lockfile_changes"])
    if tree.get("ignored_changes"):
        _write_text_path_list(file, "changed but excluded by ignore rules", tree["ignored_changes"])
    if tree.get("policy_excluded_count"):
        file.write(f"  changed files withheld by exclusion policy: {tree['policy_excluded_count']}\n")
    _write_text_raw_diff(file, tree)
    for frag in tree.get("fragments", []):
        _write_text_fragment(file, frag, "  ")


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

    if tree_type == "diff_context":
        file.write(f"diff context: {name}\n")
    elif tree_type == "file":
        file.write(f"{name}\n")
    else:
        file.write(f"{name}/\n")

    if tree_type == "diff_context" and _has_diff_metadata(tree):
        _write_tree_text_diff_context(file, tree)
    elif tree.get("children"):
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


def _escape_md_inline_code(s: str) -> str:
    if "`" not in s:
        return f"`{s}`"
    matches = _BACKTICK_RUN_PATTERN.findall(s)
    max_run = max(len(m) for m in matches) if matches else 0
    fence = "`" * (max_run + 1)
    return f"{fence} {s} {fence}"


def _write_markdown_fragment(file: TextIO, frag: dict[str, Any]) -> None:
    path = frag.get("path", "")
    lines = frag.get("lines", "")
    kind = frag.get("kind", "")
    symbol = frag.get("symbol", "")

    header = _escape_md_inline_code(f"{path}:{lines}")
    if symbol:
        header += f" **{_escape_markdown(symbol)}**"
    if kind:
        header += f" _{_escape_markdown(kind)}_"
    if frag.get("role") == "changed":
        header = f"{header} — **changed**"
    file.write(f"## {header}\n\n")

    if frag.get("content"):
        lang = _infer_language(PurePosixPath(path).name)
        _write_md_code_block(file, frag["content"], lang, "")


def _omitted_changed_files(tree: dict[str, Any]) -> list[str]:
    # The engine's inventory row says whether anything of a changed file made
    # it out; the fallback re-derives it for a dict without one.
    if tree.get("changes"):
        return [str(c["path"]) for c in tree["changes"] if not c.get("represented", True) and not c.get("no_fragments")]
    changed = tree.get("changed_files") or []
    if not changed:
        return []
    represented = {str(frag.get("path", "")) for frag in tree.get("fragments") or []}
    return [str(p) for p in changed if str(p) not in represented]


def _no_fragment_changed_files(tree: dict[str, Any]) -> list[str]:
    return [str(c["path"]) for c in tree.get("changes") or [] if c.get("no_fragments")]


def _write_md_path_list(file: TextIO, tree: dict[str, Any], key: str, title: str) -> None:
    if not tree.get(key):
        return
    file.write(f"**{title}:**\n\n")
    for path in tree[key]:
        file.write(f"- {_escape_md_inline_code(str(path))}\n")
    file.write("\n")


# The changed-file list used to be printed twice — once in full at the top, once
# again at the bottom as "not represented" — and on an 83-file range that second
# copy was ~1k tokens of paths the reader had already been given (#241). One
# list, with the omitted entries marked, carries the same two facts for a marker
# per entry instead of a whole line.
_OMITTED_MARK = " — omitted"
_NO_FRAGMENTS_MARK = " — no fragments"


def _coverage_note(tree: dict[str, Any]) -> str | None:
    coverage = tree.get("coverage")
    if not coverage:
        return None
    reasons = ", ".join(str(r) for r in coverage.get("limit_reasons") or [])
    return f"Coverage: {coverage.get('status', 'partial')} — the run hit a limit ({reasons}); context may be missing."


def _write_md_changed_files(file: TextIO, tree: dict[str, Any]) -> None:
    changed = tree.get("changed_files") or []
    if not changed:
        return
    omitted = set(_omitted_changed_files(tree))
    fragmentless = set(_no_fragment_changed_files(tree))
    file.write("**Changed files:**\n\n")
    for path in changed:
        text = str(path)
        mark = _NO_FRAGMENTS_MARK if text in fragmentless else _OMITTED_MARK if text in omitted else ""
        file.write(f"- {_escape_md_inline_code(text)}{mark}\n")
    if omitted:
        file.write("\n*\u201comitted\u201d = no fragment of this file is in the output (budget/selection).*\n")
    if fragmentless:
        file.write(
            "\n*\u201cno fragments\u201d = the file yielded nothing to select (not code, binary, or over the size cap).*\n"
        )
    file.write("\n")


def _write_md_commit_messages(file: TextIO, tree: dict[str, Any]) -> None:
    # A range is titled by all of its commits, subject and body, newest
    # first — not by the subject of the one that happens to be last (#263).
    messages = tree.get("commit_messages")
    if not messages:
        if tree.get("commit_message"):
            file.write(f"> {tree['commit_message']}\n\n")
        return
    if len(messages) == 1:
        _write_md_quoted(file, str(messages[0]), "> ")
    else:
        file.write(f"> {_commit_heading(tree, len(messages))}:\n")
        for message in messages:
            subject, _, body = str(message).partition("\n")
            file.write(f"> - **{subject}**\n")
            _write_md_quoted(file, body.strip("\n"), ">   ")
    file.write("\n")


def _write_md_quoted(file: TextIO, text: str, prefix: str) -> None:
    for line in text.splitlines():
        file.write(f"{prefix}{line}\n" if line else ">\n")


def _write_md_renamed_files(file: TextIO, tree: dict[str, Any]) -> None:
    if not tree.get("renamed_files"):
        return
    file.write("**Renamed files:**\n\n")
    for pair in tree["renamed_files"]:
        old_p = _escape_md_inline_code(str(pair.get("from", "")))
        new_p = _escape_md_inline_code(str(pair.get("to", "")))
        file.write(f"- {old_p} \u2192 {new_p}\n")
    file.write("\n")


def _write_markdown_diff_context(file: TextIO, tree: dict[str, Any]) -> None:
    _write_md_commit_messages(file, tree)
    if note := _coverage_note(tree):
        file.write(f"*{note}*\n\n")
    _write_md_changed_files(file, tree)
    _write_md_path_list(file, tree, "deleted_files", "Deleted files")
    _write_md_renamed_files(file, tree)
    _write_md_path_list(file, tree, "lockfile_changes", "Lock files changed")
    _write_md_path_list(file, tree, "ignored_changes", "Changed but excluded by ignore rules")
    if tree.get("policy_excluded_count"):
        n = tree["policy_excluded_count"]
        file.write(f"*{n} changed file(s) withheld by exclusion policy (`.diffctx/ignore` or secret paths).*\n\n")
    if tree.get("raw_diff"):
        file.write("## Raw diff\n\n")
        _write_md_code_block(file, tree["raw_diff"], "diff", "")
    for frag in tree.get("fragments", []):
        _write_markdown_fragment(file, frag)


def write_tree_markdown(file: TextIO, tree: dict[str, Any]) -> None:
    name = tree.get("name", "")
    tree_type = tree.get("type", "")
    if tree_type == "diff_context":
        file.write(f"# diff context: {name}\n\n")
    elif tree_type == "file":
        file.write(f"# {name}\n\n")
    else:
        file.write(f"# {name}/\n\n")

    if tree_type == "diff_context" and _has_diff_metadata(tree):
        _write_markdown_diff_context(file, tree)
    elif tree.get("children"):
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


def _render(tree: dict[str, Any], output_format: str) -> str:
    buf = io.StringIO()
    _WRITERS.get(output_format, write_tree_yaml)(buf, tree)
    return buf.getvalue()


_CLASS_DROP_ORDER = {"generated": 0, "mechanical": 1, "unknown": 2, "content": 3}


def _drop_index(fragments: list[dict[str, Any]], classes: dict[str, str]) -> int:
    # The same order the selection policy admits in, reversed: context from
    # the tail first; then a changed file's second fragment before any file
    # loses its only one; then witnesses by class, mechanical bumps before
    # hand-written content, so a tight budget keeps what a reviewer needs.
    context = [i for i, f in enumerate(fragments) if f.get("role") != "changed"]
    if context:
        return context[-1]
    per_file: dict[str, int] = {}
    for f in fragments:
        per_file[str(f.get("path"))] = per_file.get(str(f.get("path")), 0) + 1
    seconds = [i for i, f in enumerate(fragments) if per_file[str(f.get("path"))] > 1]
    if seconds:
        return seconds[-1]
    return max(
        range(len(fragments)),
        key=lambda i: (-_CLASS_DROP_ORDER.get(classes.get(str(fragments[i].get("path")), "content"), 3), i),
    )


_CLIPPED_MARKER = re.compile(r"^… \[(\d+) more lines of this change\]$")


def _halve_witness(fragment: dict[str, Any]) -> dict[str, Any] | None:
    content = fragment.get("content")
    if not isinstance(content, str):
        return None
    lines = content.splitlines()
    hidden = 0
    marker = _CLIPPED_MARKER.match(lines[-1]) if lines else None
    if marker:
        hidden = int(marker.group(1))
        lines.pop()
    if len(lines) < 2:
        return None
    keep = min(max(len(lines) * 3 // 4, 1), len(lines) - 1)
    hidden += len(lines) - keep
    start = int(str(fragment.get("lines", "1")).split("-")[0])
    clipped = "\n".join(lines[:keep]) + f"\n… [{hidden} more lines of this change]\n"
    halved = {**fragment, "content": clipped, "lines": f"{start}-{start + keep - 1}"}
    if "token_count" in fragment:
        halved["token_count"] = count_tokens(clipped)
    return halved


def _drop_one_fragment(tree: dict[str, Any]) -> dict[str, Any]:
    fragments = list(tree["fragments"])
    classes = {str(c["path"]): str(c.get("class", "content")) for c in tree.get("changes") or []}
    index = _drop_index(fragments, classes)
    target = fragments[index]
    last_witness = target.get("role") == "changed" and sum(1 for f in fragments if f.get("path") == target.get("path")) == 1
    # A changed file's last witness is shortened, not dropped: a larger budget must never show less of the
    # change. Context has no such floor and drops whole.
    halved = _halve_witness(target) if last_witness else None
    if halved is None:
        fragments.pop(index)
    else:
        fragments[index] = halved
    represented = {str(f.get("path")) for f in fragments}
    trimmed = {**tree, "fragments": fragments, "fragment_count": len(fragments)}
    if tree.get("changes"):
        trimmed["changes"] = [{**c, "represented": str(c["path"]) in represented} for c in tree["changes"]]
    coverage = dict(tree.get("coverage") or {"status": "partial", "limit_reasons": [], "resources": {}})
    reasons = list(coverage.get("limit_reasons") or [])
    if "selection_budget_exceeded" not in reasons:
        reasons.append("selection_budget_exceeded")
    coverage["limit_reasons"] = reasons
    if any(not c["represented"] and not c.get("no_fragments") for c in trimmed.get("changes") or []):
        coverage["status"] = "degraded"
    trimmed["coverage"] = coverage
    return trimmed


def fit_to_budget(tree: dict[str, Any], output_format: str) -> tuple[dict[str, Any], str]:
    """The rendered document is what the budget bounds, so the rendering is
    where the cap is enforced: the engine's envelope charge is an estimate
    (#259), and anything it under-charges — a coverage note, a format's own
    scaffolding — would otherwise push the artifact past `--budget`. Context
    is dropped from the tail first; a changed fragment only when no context
    is left; the inventory, coverage and provenance never. A tree with no
    budget in its provenance renders as is."""
    selection = (tree.get("provenance") or {}).get("selection") or {}
    budget = selection.get("budget_tokens")
    if not isinstance(budget, int) or budget <= 0 or budget >= 10_000_000 or not tree.get("fragments"):
        return tree, _render(tree, output_format)

    rendered = _render(tree, output_format)
    while count_tokens(rendered) > budget and tree.get("fragments"):
        tree = _drop_one_fragment(tree)
        rendered = _render(tree, output_format)
    return tree, rendered


def tree_to_string(tree: dict[str, Any], output_format: str = "yaml") -> str:
    return fit_to_budget(tree, output_format)[1]


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
