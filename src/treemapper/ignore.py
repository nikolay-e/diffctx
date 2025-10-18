# src/treemapper/ignore.py
import logging
import os
from pathlib import Path
from typing import List, Optional

# pathspec doesn't have type stubs
import pathspec  # type: ignore


def read_ignore_file(file_path: Path) -> List[str]:
    """Read the ignore patterns from the specified ignore file."""
    ignore_patterns = []
    if file_path.is_file():
        try:
            # Try to read directly and handle all possible errors
            with file_path.open("r", encoding="utf-8") as f:
                ignore_patterns = [line.strip() for line in f if line.strip() and not line.startswith("#")]
            logging.info(f"Using ignore patterns from {file_path}")
            logging.debug(f"Read ignore patterns from {file_path}: {ignore_patterns}")
        except PermissionError:
            logging.warning(f"Could not read ignore file {file_path}: Permission denied")
        except IOError as e:
            logging.warning(f"Could not read ignore file {file_path}: {e}")
        except UnicodeDecodeError as e:
            logging.warning(f"Could not decode ignore file {file_path} as UTF-8: {e}")
    return ignore_patterns


def load_pathspec(patterns: List[str], syntax="gitwildmatch") -> pathspec.PathSpec:
    """Load pathspec from a list of patterns."""
    spec = pathspec.PathSpec.from_lines(syntax, patterns)
    logging.debug(f"Loaded pathspec with patterns: {patterns}")
    return spec


def _aggregate_gitignore_patterns(root: Path) -> List[str]:
    """
    Aggregate all .gitignore patterns from root and subdirectories.
    Converts patterns to root-relative paths matching Git's semantics.
    """
    out: List[str] = []
    for dirpath, dirnames, filenames in os.walk(root, topdown=True):
        # Sort dirnames and filenames in place for deterministic traversal order
        dirnames.sort()
        filenames.sort()

        if ".gitignore" not in filenames:
            continue
        gitdir = Path(dirpath)
        rel = "" if gitdir == root else gitdir.relative_to(root).as_posix()
        for line in read_ignore_file(gitdir / ".gitignore"):
            neg = line.startswith("!")
            pat = line[1:] if neg else line
            # Handle anchored patterns (starting with /)
            if pat.startswith("/"):
                full = f"/{rel}{pat}" if rel else pat
            else:
                full = f"{rel}/{pat}" if rel else pat
            out.append(("!" + full) if neg else full)
    logging.debug(f"Aggregated {len(out)} gitignore patterns from {root}")
    return out


# ---> CHANGE: Removed dead gitignore_specs return value <---
def get_ignore_specs(
    root_dir: Path,
    custom_ignore_file: Optional[Path] = None,
    no_default_ignores: bool = False,
    output_file: Optional[Path] = None,
) -> pathspec.PathSpec:
    """Get combined ignore specs. Returns combined pathspec."""
    default_patterns = get_default_patterns(root_dir, no_default_ignores, output_file)
    # ---> CHANGE: No longer pass root_dir to get_custom_patterns.
    custom_patterns = get_custom_patterns(custom_ignore_file)

    # Aggregate all .gitignore patterns into a single list
    git_patterns = [] if no_default_ignores else _aggregate_gitignore_patterns(root_dir)

    if no_default_ignores:
        combined_patterns = custom_patterns
        if output_file:
            try:
                resolved_output = output_file.resolve()
                resolved_root = root_dir.resolve()
                if resolved_output.is_relative_to(resolved_root):
                    relative_output_str = resolved_output.relative_to(resolved_root).as_posix()
                    output_pattern = f"/{relative_output_str}"
                    if output_pattern not in combined_patterns:
                        combined_patterns.append(output_pattern)
                        logging.debug(f"Adding output file to ignores (no_default_ignores=True): {output_pattern}")
            except ValueError:
                pass
            except Exception as e:
                logging.warning(f"Could not determine relative path for output file {output_file}: {e}")
    else:
        # Combine default patterns, git patterns, and custom patterns
        combined_patterns = default_patterns + git_patterns + custom_patterns

    logging.debug(f"Ignore specs params: no_default_ignores={no_default_ignores}")
    logging.debug(f"Default patterns (used unless no_default_ignores): {default_patterns}")
    logging.debug(f"Git patterns: {git_patterns}")
    logging.debug(f"Custom patterns (-i): {custom_patterns}")
    logging.debug(f"Combined patterns for spec: {combined_patterns}")

    combined_spec = load_pathspec(combined_patterns)

    return combined_spec


# ---> ИЗМЕНЕНИЕ: Заменяем | None на Optional[...] <---
def get_default_patterns(root_dir: Path, no_default_ignores: bool, output_file: Optional[Path]) -> List[str]:
    """Retrieve default ignore patterns ONLY IF no_default_ignores is FALSE."""
    if no_default_ignores:
        return []

    # Add common patterns to default ignores
    patterns = [
        # Python
        "**/__pycache__/",
        "**/*.py[cod]",
        "**/*.so",
        "**/.pytest_cache/",
        "**/.coverage",
        "**/.mypy_cache/",
        "**/*.egg-info/",
        "**/.eggs/",
        # VCS
        "**/.git/",
        # Node.js
        "**/node_modules/",
        # Python virtual environments
        "**/venv/",
        "**/.venv/",
        # Testing/build tools
        "**/.tox/",
        "**/.nox/",
        "**/dist/",
        "**/build/",
    ]

    # Look for .treemapperignore in the scanned root_dir
    treemapper_ignore_file = root_dir / ".treemapperignore"
    patterns.extend(read_ignore_file(treemapper_ignore_file))

    if output_file:
        try:
            # This logic correctly ignores the output file if it's inside the scanned directory.
            # It should remain as is.
            resolved_output = output_file.resolve()
            resolved_root = root_dir.resolve()
            try:
                relative_output = resolved_output.relative_to(resolved_root)
                output_pattern = f"/{relative_output.as_posix()}"
                patterns.append(output_pattern)
                logging.debug(f"Adding output file to default ignores: {output_pattern}")
            except ValueError:
                logging.debug(
                    f"Output file {output_file} is outside root directory {root_dir}, " "not adding to default ignores."
                )
            except Exception as e:
                logging.warning(f"Could not determine relative path for output file {output_file}: {e}")
        except Exception as e:
            logging.warning(f"Could not determine relative path for output file {output_file}: {e}")

    return patterns


# ---> CHANGE: Remove the unused 'root_dir' parameter for clarity.
def get_custom_patterns(custom_ignore_file: Optional[Path]) -> List[str]:
    """Retrieve custom ignore patterns from the file specified with -i."""
    if not custom_ignore_file:
        return []

    # No need to resolve path here, cli.py already did it.
    if custom_ignore_file.is_file():
        return read_ignore_file(custom_ignore_file)
    else:
        # This case is now handled in cli.py, but we keep it as a safeguard.
        logging.warning(f"Custom ignore file '{custom_ignore_file}' not found.")
        return []


def should_ignore(relative_path_str: str, combined_spec: pathspec.PathSpec) -> bool:
    """Check if a file or directory should be ignored based on combined pathspec."""
    is_ignored = combined_spec.match_file(relative_path_str)
    logging.debug(f"Checking combined spec ignore for '{relative_path_str}': {is_ignored}")
    return is_ignored
