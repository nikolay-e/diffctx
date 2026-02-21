from __future__ import annotations

import logging
import os
from pathlib import Path

import pathspec


def read_ignore_file(file_path: Path) -> list[str]:
    ignore_patterns: list[str] = []
    if not file_path.is_file():
        return ignore_patterns

    try:
        with file_path.open("r", encoding="utf-8") as f:
            for line in f:
                stripped = line.rstrip("\n\r")
                if not stripped.strip():
                    continue
                if stripped.startswith("#"):
                    continue
                ignore_patterns.append(stripped.rstrip())
        logging.info("Using ignore patterns from %s", file_path)
        logging.debug("Read ignore patterns from %s: %s", file_path, ignore_patterns)
    except PermissionError:
        logging.warning("Could not read ignore file %s: Permission denied", file_path)
    except OSError as e:
        logging.warning("Could not read ignore file %s: %s", file_path, e)
    except UnicodeDecodeError as e:
        logging.warning("Could not decode ignore file %s as UTF-8: %s", file_path, e)

    return ignore_patterns


def _get_output_file_pattern(output_file: Path | None, root_dir: Path) -> str | None:
    if not output_file:
        return None

    try:
        resolved_output = output_file.resolve()
        resolved_root = root_dir.resolve()

        if not resolved_output.is_relative_to(resolved_root):
            logging.debug("Output file %s is outside root directory %s", output_file, root_dir)
            return None

        relative_path = resolved_output.relative_to(resolved_root).as_posix()
        return f"/{relative_path}"
    except (ValueError, OSError) as e:
        logging.debug("Could not determine relative path for output file %s: %s", output_file, e)
        return None


PRUNE_DIRS = frozenset(
    {
        ".git",
        ".svn",
        ".hg",
        "__pycache__",
        "node_modules",
        ".npm",
        "venv",
        ".venv",
        ".tox",
        ".nox",
        "target",
        ".gradle",
        "bin",
        "obj",
        "vendor",
        "dist",
        "build",
        "out",
        ".idea",
        ".vscode",
    }
)


def _is_cache_dir(name: str) -> bool:
    return name.startswith(".") and name.endswith("_cache")


def _process_ignore_line(line: str, rel: str) -> str:
    neg = line.startswith("!")
    pat = line[1:] if neg else line

    pat_without_trailing_slash = pat.rstrip("/")
    if pat_without_trailing_slash.startswith("/") or "/" in pat_without_trailing_slash:
        anchored_pat = pat.lstrip("/")
        full = f"/{rel}/{anchored_pat}" if rel else f"/{anchored_pat}"
    elif rel:
        full = f"{rel}/**/{pat}"
    else:
        full = pat

    return ("!" + full) if neg else full


def _aggregate_all_ignore_patterns(root: Path, ignore_filenames: list[str]) -> list[str]:
    out: list[str] = []
    filenames_set = set(ignore_filenames)

    for dirpath, dirnames, filenames in os.walk(root, topdown=True):
        dirnames[:] = sorted(d for d in dirnames if d not in PRUNE_DIRS and not _is_cache_dir(d))

        found_files = filenames_set & set(filenames)
        if not found_files:
            continue

        ignore_dir = Path(dirpath)
        rel = "" if ignore_dir == root else ignore_dir.relative_to(root).as_posix()

        for ignore_filename in sorted(found_files):
            for line in read_ignore_file(ignore_dir / ignore_filename):
                out.append(_process_ignore_line(line, rel))

    logging.debug("Aggregated %d ignore patterns from %s", len(out), root)
    return out


def _transform_anchored_pattern(anchored: str, rel_to_root: str) -> str | None:
    prefix = rel_to_root + "/"
    if anchored.startswith(prefix):
        return "/" + anchored[len(prefix) :]
    if anchored in {rel_to_root, prefix}:
        return None
    return None


def _transform_relative_pattern(pat: str, rel_to_root: str) -> str | None:
    prefix = rel_to_root + "/"
    if pat.startswith(prefix):
        return "/" + pat[len(prefix) :]
    if pat in {rel_to_root, prefix}:
        return None
    return None


def _transform_parent_pattern(line: str, rel_to_root: str) -> str | None:
    neg = line.startswith("!")
    pat = line[1:] if neg else line

    result: str
    if "**" in pat or "/" not in pat:
        result = pat
    elif pat.startswith("/"):
        transformed = _transform_anchored_pattern(pat[1:], rel_to_root)
        if transformed is None:
            return None
        result = transformed
    else:
        transformed = _transform_relative_pattern(pat, rel_to_root)
        if transformed is None:
            return None
        result = transformed

    return ("!" + result) if neg else result


def _process_parent_ignore_file(ignore_file: Path, resolved_root: Path, parent_dir: Path, out: list[str]) -> None:
    if not ignore_file.is_file():
        return
    try:
        rel_to_root = resolved_root.relative_to(parent_dir).as_posix()
    except ValueError:
        return

    for line in read_ignore_file(ignore_file):
        pattern = _transform_parent_pattern(line, rel_to_root)
        if pattern:
            out.append(pattern)


def _collect_parent_ignore_patterns(root: Path, ignore_filenames: list[str]) -> list[str]:
    out: list[str] = []
    resolved_root = root.resolve()

    current = resolved_root.parent
    while current != current.parent:
        is_git_root = (current / ".git").exists()

        for filename in ignore_filenames:
            _process_parent_ignore_file(current / filename, resolved_root, current, out)

        if is_git_root:
            break

        current = current.parent

    if out:
        logging.debug("Collected %d patterns from parent directories", len(out))
    return out


DEFAULT_IGNORE_PATTERNS = [
    # Version control
    "**/.git/",
    "**/.svn/",
    "**/.hg/",
    # Python
    "**/__pycache__/",
    "**/*.py[cod]",
    "**/*.so",
    "**/.coverage",
    "**/*.egg-info/",
    "**/.eggs/",
    "**/venv/",
    "**/.venv/",
    "**/.tox/",
    "**/.nox/",
    # JavaScript/Node
    "**/node_modules/",
    "**/.npm/",
    # Java/JVM
    "**/target/",
    "**/.gradle/",
    # .NET
    "**/bin/",
    "**/obj/",
    # Go
    "**/vendor/",
    # Generic build/cache
    "**/dist/",
    "**/build/",
    "**/out/",
    "**/.*_cache/",
    # IDE
    "**/.idea/",
    "**/.vscode/",
    # OS files
    "**/.DS_Store",
    "**/Thumbs.db",
    # TreeMapper default output files
    "**/tree.yaml",
    "**/tree.yml",
    "**/tree.json",
    "**/tree.md",
    "**/tree.txt",
]


def get_ignore_specs(
    root_dir: Path,
    custom_ignore_file: Path | None = None,
    no_default_ignores: bool = False,
    output_file: Path | None = None,
) -> pathspec.PathSpec:
    patterns: list[str] = []

    if not no_default_ignores:
        patterns.extend(DEFAULT_IGNORE_PATTERNS)
        patterns.extend(_collect_parent_ignore_patterns(root_dir, [".gitignore", ".treemapperignore"]))
        patterns.extend(_aggregate_all_ignore_patterns(root_dir, [".gitignore", ".treemapperignore"]))

    if custom_ignore_file:
        patterns.extend(read_ignore_file(custom_ignore_file))

    output_pattern = _get_output_file_pattern(output_file, root_dir)
    if output_pattern and output_pattern not in patterns:
        patterns.append(output_pattern)
        logging.debug("Adding output file to ignores: %s", output_pattern)

    logging.debug("Combined ignore patterns: %s", patterns)
    spec: pathspec.PathSpec = pathspec.PathSpec.from_lines("gitignore", patterns)
    return spec


def should_ignore(relative_path_str: str, combined_spec: pathspec.PathSpec) -> bool:
    is_ignored = combined_spec.match_file(relative_path_str)
    if logging.getLogger().isEnabledFor(logging.DEBUG):
        logging.debug("Checking ignore for '%s': %s", relative_path_str, is_ignored)
    return is_ignored


DEFAULT_WHITELIST_FILENAME = ".treemapperwhitelist"


def get_whitelist_spec(whitelist_file: Path | None, root_dir: Path | None = None) -> pathspec.PathSpec | None:
    effective_file = whitelist_file
    if not effective_file and root_dir:
        default = root_dir / DEFAULT_WHITELIST_FILENAME
        if default.is_file():
            effective_file = default
    if not effective_file:
        return None
    patterns = read_ignore_file(effective_file)
    if not patterns:
        return None
    return pathspec.PathSpec.from_lines("gitignore", patterns)


def is_whitelisted(relative_path_str: str, whitelist_spec: pathspec.PathSpec | None, is_dir: bool = False) -> bool:
    if whitelist_spec is None:
        return True
    if is_dir:
        return True
    return whitelist_spec.match_file(relative_path_str)
