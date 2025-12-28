from __future__ import annotations

import io
import json
import logging
import re
import sys
from pathlib import Path
from typing import Any, TextIO

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

EXTENSION_TO_LANG = {
    ".py": "python",
    ".pyw": "python",
    ".pyi": "python",
    ".js": "javascript",
    ".mjs": "javascript",
    ".cjs": "javascript",
    ".jsx": "jsx",
    ".ts": "typescript",
    ".tsx": "tsx",
    ".mts": "typescript",
    ".cts": "typescript",
    ".json": "json",
    ".yaml": "yaml",
    ".yml": "yaml",
    ".toml": "toml",
    ".md": "markdown",
    ".markdown": "markdown",
    ".html": "html",
    ".htm": "html",
    ".css": "css",
    ".scss": "scss",
    ".sass": "sass",
    ".less": "less",
    ".xml": "xml",
    ".svg": "xml",
    ".sh": "bash",
    ".bash": "bash",
    ".zsh": "zsh",
    ".fish": "fish",
    ".ps1": "powershell",
    ".psm1": "powershell",
    ".bat": "batch",
    ".cmd": "batch",
    ".c": "c",
    ".h": "c",
    ".cpp": "cpp",
    ".cc": "cpp",
    ".cxx": "cpp",
    ".hpp": "cpp",
    ".hxx": "cpp",
    ".cs": "csharp",
    ".java": "java",
    ".kt": "kotlin",
    ".kts": "kotlin",
    ".scala": "scala",
    ".go": "go",
    ".rs": "rust",
    ".rb": "ruby",
    ".php": "php",
    ".swift": "swift",
    ".m": "objectivec",
    ".mm": "objectivec",
    ".r": "r",
    ".R": "r",
    ".lua": "lua",
    ".pl": "perl",
    ".pm": "perl",
    ".ex": "elixir",
    ".exs": "elixir",
    ".erl": "erlang",
    ".hrl": "erlang",
    ".hs": "haskell",
    ".lhs": "haskell",
    ".ml": "ocaml",
    ".mli": "ocaml",
    ".clj": "clojure",
    ".cljs": "clojure",
    ".cljc": "clojure",
    ".sql": "sql",
    ".graphql": "graphql",
    ".gql": "graphql",
    ".proto": "protobuf",
    ".dockerfile": "dockerfile",
    ".tf": "hcl",
    ".hcl": "hcl",
    ".vim": "vim",
    ".el": "elisp",
    ".lisp": "lisp",
    ".scm": "scheme",
    ".rkt": "racket",
    ".zig": "zig",
    ".nim": "nim",
    ".v": "v",
    ".d": "d",
    ".dart": "dart",
    ".groovy": "groovy",
    ".gradle": "groovy",
    ".ini": "ini",
    ".cfg": "ini",
    ".conf": "ini",
    ".properties": "properties",
    ".env": "dotenv",
    ".gitignore": "gitignore",
    ".dockerignore": "gitignore",
    ".editorconfig": "editorconfig",
    ".tex": "latex",
    ".latex": "latex",
    ".rst": "rst",
    ".txt": "text",
    ".log": "text",
    ".diff": "diff",
    ".patch": "diff",
}

FILENAME_TO_LANG = {
    "makefile": "makefile",
    "gnumakefile": "makefile",
    "dockerfile": "dockerfile",
    "containerfile": "dockerfile",
    "vagrantfile": "ruby",
    "gemfile": "ruby",
    "rakefile": "ruby",
    "guardfile": "ruby",
    "brewfile": "ruby",
    "podfile": "ruby",
    "cmakelists.txt": "cmake",
    "justfile": "just",
    ".bashrc": "bash",
    ".bash_profile": "bash",
    ".bash_aliases": "bash",
    ".zshrc": "zsh",
    ".zshenv": "zsh",
    ".zprofile": "zsh",
    ".profile": "bash",
    ".gitconfig": "gitconfig",
    ".gitattributes": "gitattributes",
    ".gitignore": "gitignore",
    ".dockerignore": "gitignore",
    ".treemapperignore": "gitignore",
    ".npmrc": "ini",
    ".yarnrc": "yaml",
    ".prettierrc": "json",
    ".eslintrc": "json",
    "package.json": "json",
    "tsconfig.json": "json",
    "composer.json": "json",
    "cargo.toml": "toml",
    "pyproject.toml": "toml",
    "go.mod": "gomod",
    "go.sum": "gosum",
    "requirements.txt": "text",
    "pipfile": "toml",
    "procfile": "text",
}

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


def _write_yaml_node(file: TextIO, node: dict[str, Any], indent: str = "") -> None:
    name = _escape_yaml_string(str(node["name"]))
    file.write(f'{indent}- name: "{name}"\n')
    file.write(f"{indent}  type: {node['type']}\n")

    if "content" in node:
        content = node["content"]
        if content:
            if _has_problematic_chars(content):
                escaped = _escape_yaml_content(content)
                file.write(f'{indent}  content: "{escaped}"\n')
            else:
                file.write(f"{indent}  content: |\n")
                lines = content.rstrip("\n").split("\n")
                for line in lines:
                    file.write(f"{indent}    {line}\n")
        else:
            file.write(f'{indent}  content: ""\n')

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


def write_tree_text(file: TextIO, tree: dict[str, Any]) -> None:
    name = tree.get("name", "")
    file.write(f"{name}/\n")

    if tree.get("children"):
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
    name_lower = filename.lower()
    if name_lower in FILENAME_TO_LANG:
        return FILENAME_TO_LANG[name_lower]
    ext = Path(filename).suffix.lower()
    return EXTENSION_TO_LANG.get(ext, "")


def _get_fence_length(content: str) -> int:
    matches = _BACKTICK_RUN_PATTERN.findall(content)
    if not matches:
        return 3
    return max(3, max(len(m) for m in matches) + 1)


def _write_markdown_node(file: TextIO, node: dict[str, Any], depth: int) -> None:
    name = node.get("name", "")
    node_type = node.get("type", "")
    is_dir = node_type == "directory"
    display_name = f"{name}/" if is_dir else name

    in_list = depth > 5
    list_indent = "  " * (depth - 5) if in_list else ""
    content_indent = list_indent + "  " if in_list else ""

    if depth <= 5:
        heading = "#" * (depth + 1)
        file.write(f"{heading} {display_name}\n\n")
    else:
        file.write(f"{list_indent}- **{display_name}**\n\n")

    if "content" in node:
        content = node["content"]
        if content:
            if _is_placeholder(content):
                file.write(f"{content_indent}_{content.strip()}_\n\n")
            else:
                lang = _infer_language(name)
                fence_len = _get_fence_length(content)
                fence = "`" * fence_len
                file.write(f"{content_indent}{fence}{lang}\n")
                for line in content.splitlines(keepends=True):
                    file.write(f"{content_indent}{line}")
                if not content.endswith("\n"):
                    file.write("\n")
                file.write(f"{content_indent}{fence}\n\n")
    elif is_dir and not node.get("children"):
        file.write(f"{content_indent}_(empty directory)_\n\n")

    if node.get("children"):
        for child in node["children"]:
            _write_markdown_node(file, child, depth + 1)


def write_tree_markdown(file: TextIO, tree: dict[str, Any]) -> None:
    name = tree.get("name", "")
    file.write(f"# {name}/\n\n")

    if tree.get("children"):
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


def write_string_to_file(content: str, output_file: Path | None, output_format: str = "yaml") -> None:
    if output_file is None:
        try:
            buf = sys.stdout.buffer
        except AttributeError:
            buf = None

        try:
            if buf:
                utf8_stdout = io.TextIOWrapper(buf, encoding="utf-8", newline="")
                try:
                    utf8_stdout.write(content)
                    utf8_stdout.flush()
                finally:
                    utf8_stdout.detach()
            else:
                sys.stdout.write(content)
                sys.stdout.flush()
        except BrokenPipeError:
            pass

        logging.info(f"Directory tree written to stdout in {output_format} format")
    else:
        try:
            output_file.parent.mkdir(parents=True, exist_ok=True)

            if output_file.is_dir():
                logging.error(f"Cannot write to '{output_file}': is a directory")
                raise IsADirectoryError(f"Is a directory: {output_file}")

            with output_file.open("w", encoding="utf-8") as f:
                f.write(content)
            logging.info(f"Directory tree saved to {output_file} in {output_format} format")
        except PermissionError:
            logging.error(f"Unable to write to file '{output_file}': Permission denied")
            raise
        except OSError as e:
            logging.error(f"Unable to write to file '{output_file}': {e}")
            raise


def write_tree_to_file(tree: dict[str, Any], output_file: Path | None, output_format: str = "yaml") -> None:
    def write_tree_content(f: TextIO) -> None:
        if output_format == "json":
            write_tree_json(f, tree)
        elif output_format == "txt":
            write_tree_text(f, tree)
        elif output_format == "md":
            write_tree_markdown(f, tree)
        else:  # yaml
            write_tree_yaml(f, tree)

    if output_file is None:
        try:
            buf = sys.stdout.buffer
        except AttributeError:
            buf = None

        try:
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
        except BrokenPipeError:
            pass

        logging.info(f"Directory tree written to stdout in {output_format} format")
    else:
        try:
            output_file.parent.mkdir(parents=True, exist_ok=True)

            if output_file.is_dir():
                logging.error(f"Cannot write to '{output_file}': is a directory")
                raise IsADirectoryError(f"Is a directory: {output_file}")

            with output_file.open("w", encoding="utf-8") as f:
                write_tree_content(f)
            logging.info(f"Directory tree saved to {output_file} in {output_format} format")
        except PermissionError:
            logging.error(f"Unable to write to file '{output_file}': Permission denied")
            raise
        except OSError as e:
            logging.error(f"Unable to write to file '{output_file}': {e}")
            raise
