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
        logging.warning("Permission denied accessing directory %s", dir_path)
    except OSError as e:
        logging.warning("Error accessing directory %s: %s", dir_path, e)

    return tree


def _process_entry(entry: Path, ctx: TreeBuildContext, current_depth: int) -> dict[str, Any] | None:
    try:
        relative_path = entry.relative_to(ctx.base_dir).as_posix()
        is_dir = entry.is_dir()
    except (OSError, ValueError) as e:
        logging.warning("Could not process path for entry %s: %s", entry, e)
        return None

    if ctx.is_output_file(entry):
        logging.debug("Skipping output file: %s", entry)
        return None

    path_to_check = relative_path + "/" if is_dir else relative_path
    if should_ignore(path_to_check, ctx.combined_spec):
        return None

    if entry.is_symlink() or not entry.exists():
        logging.debug("Skipping '%s': symlink or not exists", path_to_check)
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
    except OSError as e:
        logging.error("Failed to create node for %s: %s", entry.name, e)
        return None


def _format_binary_placeholder(file_size: int) -> str:
    return f"<binary file: {file_size} bytes>\n"


def _format_size_placeholder(file_size: int) -> str:
    return f"<file too large: {file_size} bytes>\n"


def _detect_binary_in_sample(file_path: Path, file_size: int) -> tuple[bytes | None, str | None]:
    with file_path.open("rb") as f:
        raw_bytes = f.read()
    if b"\x00" in raw_bytes[:BINARY_DETECTION_SAMPLE_SIZE]:
        logging.debug("Detected binary file %s", file_path.name)
        return None, _format_binary_placeholder(file_size)
    return raw_bytes, None


def _decode_file_content(raw_bytes: bytes, file_path: Path, file_size: int) -> str:
    if b"\x00" in raw_bytes[BINARY_DETECTION_SAMPLE_SIZE:]:
        logging.debug("Detected binary file %s (null in remainder)", file_path.name)
        return _format_binary_placeholder(file_size)

    content = raw_bytes.decode("utf-8")
    content = content.replace("\r\n", "\n").replace("\r", "\n")
    if not content:
        return ""
    return content if content.endswith("\n") else content + "\n"


def _read_file_content(file_path: Path, max_file_bytes: int | None) -> str:
    try:
        file_size = file_path.stat().st_size

        if file_path.suffix.lower() in KNOWN_BINARY_EXTENSIONS:
            logging.debug("Skipping known binary extension: %s", file_path.name)
            return _format_binary_placeholder(file_size)

        effective_limit = max_file_bytes if max_file_bytes is not None else MAX_SAFE_FILE_SIZE
        if file_size > effective_limit:
            logging.info("Skipping large file %s: %d bytes > %d bytes", file_path.name, file_size, effective_limit)
            return _format_size_placeholder(file_size)

        raw_bytes, binary_result = _detect_binary_in_sample(file_path, file_size)
        if binary_result is not None:
            return binary_result

        assert raw_bytes is not None
        return _decode_file_content(raw_bytes, file_path, file_size)

    except PermissionError:
        logging.error("Could not read %s: Permission denied", file_path.name)
        return "<unreadable content>\n"
    except UnicodeDecodeError:
        logging.error("Cannot decode %s as UTF-8. Marking as unreadable.", file_path.name)
        return "<unreadable content: not utf-8>\n"
    except OSError as e:
        logging.error("Could not read %s: %s", file_path.name, e)
        return "<unreadable content>\n"
