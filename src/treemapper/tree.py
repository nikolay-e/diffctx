from __future__ import annotations

import logging
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import pathspec

from .ignore import should_ignore

BINARY_DETECTION_SAMPLE_SIZE = 8192
MAX_SAFE_FILE_SIZE = 100 * 1024 * 1024  # 100 MB - prevent OOM when --max-file-bytes 0

KNOWN_BINARY_EXTENSIONS = frozenset(
    {
        ".pdf",
        ".doc",
        ".docx",
        ".xls",
        ".xlsx",
        ".ppt",
        ".pptx",
        ".odt",
        ".ods",
        ".odp",
        ".rtf",
        ".zip",
        ".tar",
        ".gz",
        ".bz2",
        ".xz",
        ".7z",
        ".rar",
        ".png",
        ".jpg",
        ".jpeg",
        ".gif",
        ".bmp",
        ".ico",
        ".webp",
        ".svg",
        ".tiff",
        ".tif",
        ".mp3",
        ".mp4",
        ".avi",
        ".mov",
        ".mkv",
        ".flv",
        ".wmv",
        ".wav",
        ".flac",
        ".ogg",
        ".m4a",
        ".exe",
        ".dll",
        ".so",
        ".dylib",
        ".bin",
        ".dmg",
        ".iso",
        ".img",
        ".ttf",
        ".otf",
        ".woff",
        ".woff2",
        ".eot",
        ".sqlite",
        ".db",
        ".mdb",
        ".class",
        ".jar",
        ".war",
        ".ear",
        ".o",
        ".a",
        ".lib",
        ".obj",
        ".pyc",
        ".pyo",
        ".pyd",
        ".whl",
        ".egg",
        ".deb",
        ".rpm",
        ".apk",
        ".ipa",
        ".sketch",
        ".fig",
        ".psd",
        ".ai",
        ".eps",
        ".heic",
        ".heif",
        ".raw",
        ".cr2",
        ".nef",
        ".arw",
    }
)


@dataclass
class TreeBuildContext:
    base_dir: Path
    combined_spec: pathspec.PathSpec
    output_file: Path | None = None
    max_depth: int | None = None
    no_content: bool = False
    max_file_bytes: int | None = None
    _resolved_output_file: Path | None = None

    def __post_init__(self) -> None:
        if self.output_file:
            try:
                self._resolved_output_file = self.output_file.resolve()
            except (OSError, RuntimeError):
                self._resolved_output_file = None

    def is_output_file(self, entry: Path) -> bool:
        if not self._resolved_output_file:
            return False
        try:
            return entry.resolve() == self._resolved_output_file
        except (OSError, RuntimeError):
            return False


def build_tree(dir_path: Path, ctx: TreeBuildContext, current_depth: int = 0) -> list[dict[str, Any]]:
    if ctx.max_depth is not None and current_depth >= ctx.max_depth:
        return []

    tree: list[dict[str, Any]] = []

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


def _process_entry(entry: Path, ctx: TreeBuildContext, current_depth: int) -> dict[str, Any] | None:
    try:
        relative_path = entry.relative_to(ctx.base_dir).as_posix()
        is_dir = entry.is_dir()
    except (OSError, ValueError) as e:
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

    return _create_node(entry, ctx, current_depth, is_dir)


def _create_node(entry: Path, ctx: TreeBuildContext, current_depth: int, is_dir: bool) -> dict[str, Any] | None:
    try:
        node: dict[str, Any] = {"name": entry.name, "type": "directory" if is_dir else "file"}

        if is_dir:
            children = build_tree(entry, ctx, current_depth + 1)
            if children:
                node["children"] = children
        elif not ctx.no_content:
            node["content"] = _read_file_content(entry, ctx.max_file_bytes)

        return node
    except (OSError, PermissionError) as e:
        logging.error(f"Failed to create node for {entry.name}: {e}")
        return None


def _read_file_content(file_path: Path, max_file_bytes: int | None) -> str:
    try:
        file_size = file_path.stat().st_size

        if file_path.suffix.lower() in KNOWN_BINARY_EXTENSIONS:
            if logging.getLogger().isEnabledFor(logging.DEBUG):
                logging.debug(f"Skipping known binary extension: {file_path.name}")
            return f"<binary file: {file_size} bytes>\n"

        effective_limit = max_file_bytes if max_file_bytes is not None else MAX_SAFE_FILE_SIZE
        if file_size > effective_limit:
            if logging.getLogger().isEnabledFor(logging.INFO):
                logging.info(f"Skipping large file {file_path.name}: {file_size} bytes > {effective_limit} bytes")
            return f"<file too large: {file_size} bytes>\n"

        with file_path.open("rb") as f:
            sample = f.read(BINARY_DETECTION_SAMPLE_SIZE)
            if b"\x00" in sample:
                if logging.getLogger().isEnabledFor(logging.DEBUG):
                    logging.debug(f"Detected binary file {file_path.name}")
                return f"<binary file: {file_size} bytes>\n"
            rest = f.read()
            raw_bytes = sample + rest if rest else sample

        if b"\x00" in raw_bytes[BINARY_DETECTION_SAMPLE_SIZE:]:
            if logging.getLogger().isEnabledFor(logging.DEBUG):
                logging.debug(f"Detected binary file {file_path.name} (null in remainder)")
            return f"<binary file: {file_size} bytes>\n"

        content = raw_bytes.decode("utf-8")
        content = content.replace("\r\n", "\n").replace("\r", "\n")

        if not content:
            return ""
        return content if content.endswith("\n") else content + "\n"

    except PermissionError:
        logging.error(f"Could not read {file_path.name}: Permission denied")
        return "<unreadable content>\n"
    except UnicodeDecodeError:
        logging.error(f"Cannot decode {file_path.name} as UTF-8. Marking as unreadable.")
        return "<unreadable content: not utf-8>\n"
    except OSError as e:
        logging.error(f"Could not read {file_path.name}: {e}")
        return "<unreadable content>\n"
