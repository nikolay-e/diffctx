import argparse
import os
import sys
import logging
from pathlib import Path
from typing import List, Dict, Any

import pathspec


def setup_logging(verbosity: int):
    """Configure the logging level based on verbosity."""
    if verbosity >= 3:
        level = logging.DEBUG
    elif verbosity == 2:
        level = logging.INFO
    elif verbosity == 1:
        level = logging.WARNING
    else:
        level = logging.ERROR

    logging.basicConfig(
        level=level,
        format='%(levelname)s: %(message)s'
    )


def read_ignore_file(file_path: Path) -> List[str]:
    """Read the ignore patterns from the specified ignore file."""
    ignore_patterns = []
    if file_path.is_file():
        with file_path.open('r') as f:
            for line in f:
                line = line.strip()
                if line and not line.startswith('#'):
                    ignore_patterns.append(line)
    logging.info(f"Using ignore patterns from {file_path}")
    logging.debug(f"Read ignore patterns from {file_path}: {ignore_patterns}")
    return ignore_patterns


def load_pathspec(
        patterns: List[str],
        syntax='gitwildmatch') -> pathspec.PathSpec:
    """Load pathspec from a list of patterns."""
    spec = pathspec.PathSpec.from_lines(syntax, patterns)
    logging.debug(f"Loaded pathspec with patterns: {patterns}")
    return spec


def should_ignore(file_path: str, combined_spec: pathspec.PathSpec) -> bool:
    """
    Check if a file or directory should be ignored based on combined pathspec.
    Appends a trailing slash for directories to match directory-specific patterns.
    """
    paths_to_check = [file_path]

    # Append '/' if the file_path represents a directory
    if file_path.endswith('/'):
        paths_to_check.append(file_path)

    # Append trailing slash for all parent directories
    for part in Path(file_path).parents:
        if part != Path('.'):
            paths_to_check.append(part.as_posix() + '/')

    # Check if any pattern matches
    result = any(combined_spec.match_file(path) for path in paths_to_check)
    logging.debug(
        f"Should ignore '{file_path}': {result} (checking paths: {paths_to_check})")
    return result


def build_tree(dir_path: Path,
               base_dir: Path,
               combined_spec: pathspec.PathSpec,
               gitignore_specs: Dict[Path, pathspec.PathSpec]) -> List[Dict[str, Any]]:
    """Build the directory tree structure."""
    tree = []
    try:
        for entry in sorted(dir_path.iterdir()):
            relative_path = entry.relative_to(base_dir).as_posix()
            if entry.is_dir():
                relative_path += '/'  # Append '/' for directories

            # Explicit check for .git directory
            if entry.name == '.git':
                logging.debug(f"Skipping .git directory: {relative_path}")
                continue

            if should_ignore(relative_path, combined_spec):
                logging.debug(f"Ignoring '{relative_path}' based on combined_spec")
                continue

            # Check if the entry should be ignored based on any applicable
            # .gitignore
            ignore_entry = False
            for git_dir, git_spec in gitignore_specs.items():
                try:
                    if entry.is_relative_to(git_dir):
                        rel_path = entry.relative_to(git_dir).as_posix()
                        if entry.is_dir():
                            rel_path += '/'
                        if git_spec.match_file(rel_path):
                            logging.debug(
                                f"Ignoring '{relative_path}' based on .gitignore in '{git_dir}'")
                            ignore_entry = True
                            break
                except ValueError:
                    # entry is not relative to git_dir
                    continue
            if ignore_entry:
                continue

            if not entry.exists() or entry.is_symlink():
                logging.debug(
                    f"Skipping '{relative_path}' as it does not exist or is a symlink")
                continue

            node = {
                "name": entry.name,
                "type": "directory" if entry.is_dir() else "file"
            }

            if entry.is_dir():
                children = build_tree(
                    entry, base_dir, combined_spec, gitignore_specs)
                if children:
                    node["children"] = children
            elif entry.is_file():
                try:
                    node["content"] = entry.read_text(encoding='utf-8')
                except UnicodeDecodeError:
                    node["content"] = entry.read_bytes().decode(
                        'utf-8', errors='replace')
                except IOError:
                    node["content"] = "<unreadable content>"

            tree.append(node)
            logging.debug(f"Added node: {node}")
    except (PermissionError, OSError) as e:
        logging.warning(f"Error accessing {dir_path}: {e}")

    return tree


def write_yaml_node(file, node: Dict[str, Any], indent: str = '') -> None:
    """Write a node of the directory tree in YAML format."""
    file.write(f"{indent}- name: {node['name']}\n")
    file.write(f"{indent}  type: {node['type']}\n")

    if 'content' in node:
        file.write(f"{indent}  content: |\n")
        content_lines = node['content'].splitlines()
        for line in content_lines:
            file.write(f"{indent}    {line}\n")

    if 'children' in node and node['children']:
        file.write(f"{indent}  children:\n")
        for child in node['children']:
            write_yaml_node(file, child, indent + '  ')


def main():
    parser = argparse.ArgumentParser(
        description="Generate a YAML representation of a directory structure.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter)
    parser.add_argument(
        "directory",
        nargs="?",
        default=".",
        help="The directory to analyze")
    parser.add_argument("-i", "--ignore-file", default=None,
                        help="Path to the custom ignore file (optional)")
    parser.add_argument(
        "-o",
        "--output-file",
        default="./directory_tree.yaml",
        help="Path to the output YAML file")
    parser.add_argument("--no-git-ignore", action="store_true",
                        help="Disable git-related default ignores")
    parser.add_argument(
        "-v", "--verbosity",
        type=int,
        choices=range(0, 4),
        default=2,
        metavar="[0-3]",
        help="Set verbosity level (0: ERROR, 1: WARNING, 2: INFO, 3: DEBUG)")
    args = parser.parse_args()

    setup_logging(args.verbosity)

    root_dir = Path(args.directory).resolve()
    logging.info(f"Analyzing directory: {root_dir}")

    if not root_dir.is_dir():
        logging.error(f"Error: The path '{root_dir}' is not a valid directory.")
        sys.exit(1)

    # Read default .treemapperignore
    default_ignore_file = root_dir / '.treemapperignore'
    default_patterns = read_ignore_file(default_ignore_file)
    logging.debug(f"Default ignore patterns: {default_patterns}")

    # Read custom ignore file if provided
    if args.ignore_file:
        custom_ignore_file = Path(args.ignore_file)
        if not custom_ignore_file.is_absolute():
            custom_ignore_file = root_dir / custom_ignore_file
        if custom_ignore_file.is_file():
            custom_patterns = read_ignore_file(custom_ignore_file)
            logging.debug(f"Custom ignore patterns: {custom_patterns}")
        else:
            logging.warning(
                f"Custom ignore file '{custom_ignore_file}' not found. Proceeding without custom ignore patterns.")
            custom_patterns = []
    else:
        custom_patterns = []

    # Add default patterns
    # Add default .git ignore unless disabled
    if not args.no_git_ignore:
        # Explicitly ignore the .git directory
        default_patterns.append('.git/')
        default_patterns.append('.git/**')  # Ignore all contents within .git
        logging.debug("Added default .git ignore patterns")

    # Combine all patterns
    combined_patterns = default_patterns + custom_patterns
    logging.debug(f"Combined ignore patterns: {combined_patterns}")

    # Load pathspec with combined patterns
    combined_spec = load_pathspec(combined_patterns)

    # Find all .gitignore files and create pathspecs for each
    gitignore_specs = {}
    if not args.no_git_ignore:
        for dirpath, dirnames, filenames in os.walk(root_dir):
            if '.gitignore' in filenames:
                gitignore_path = Path(dirpath) / '.gitignore'
                gitignore_patterns = read_ignore_file(gitignore_path)
                gitignore_specs[Path(dirpath)] = load_pathspec(
                    gitignore_patterns)
                logging.debug(
                    f"Loaded .gitignore from '{gitignore_path}' with patterns: {gitignore_patterns}")

    output_file = Path(args.output_file)
    if not output_file.is_absolute():
        output_file = Path.cwd() / output_file

    directory_tree = {
        "name": root_dir.name,
        "type": "directory",
        "children": build_tree(
            root_dir,
            root_dir,
            combined_spec,
            gitignore_specs)}

    try:
        with output_file.open('w', encoding='utf-8') as f:
            f.write(f"name: {directory_tree['name']}\n")
            f.write(f"type: {directory_tree['type']}\n")
            if 'children' in directory_tree and directory_tree['children']:
                f.write("children:\n")
                for child in directory_tree['children']:
                    write_yaml_node(f, child, '  ')
        logging.info(f"Directory tree saved to {output_file}")
    except IOError as e:
        logging.error(f"Unable to write to file '{output_file}': {e}")
        sys.exit(1)


if __name__ == "__main__":
    main()
