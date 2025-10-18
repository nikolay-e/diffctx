# src/treemapper/cli.py
import argparse
import sys
from pathlib import Path
from typing import Optional, Tuple

from .version import __version__


# ---> CHANGE: Updated return type to include format, max_depth, no_content, and max_file_bytes parameters.
def parse_args() -> Tuple[Path, Optional[Path], Optional[Path], bool, int, str, Optional[int], bool, Optional[int]]:
    """Parse command line arguments."""
    parser = argparse.ArgumentParser(
        prog="treemapper",
        description="Generate a structured representation of a directory tree (YAML, JSON, or text).",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )

    parser.add_argument("--version", action="version", version=f"%(prog)s {__version__}")

    parser.add_argument("directory", nargs="?", default=".", help="The directory to analyze")

    parser.add_argument("-i", "--ignore-file", default=None, help="Path to the custom ignore file (optional)")

    # ---> CHANGE: Default output is now stdout (None). Only write to file when explicitly specified.
    parser.add_argument("-o", "--output-file", default=None, help="Path to the output file (default: stdout)")

    parser.add_argument(
        "--format",
        choices=["yaml", "json", "text"],
        default="yaml",
        help="Output format: yaml, json, or text (default: yaml)",
    )

    parser.add_argument(
        "--no-default-ignores",
        action="store_true",
        help="Disable default ignores (.treemapperignore, .gitignore, output file)",
    )

    parser.add_argument(
        "--max-depth",
        type=int,
        default=None,
        metavar="N",
        help="Maximum depth to traverse (default: unlimited)",
    )

    parser.add_argument("--no-content", action="store_true", help="Skip reading file contents (structure-only mode)")

    parser.add_argument(
        "--max-file-bytes",
        type=int,
        default=None,
        metavar="N",
        help="Maximum file size to read in bytes (default: unlimited). Larger files will show a placeholder.",
    )

    parser.add_argument(
        "-v",
        "--verbosity",
        type=int,
        choices=range(0, 4),
        default=0,
        metavar="[0-3]",
        help="Set verbosity level (0: ERROR, 1: WARNING, 2: INFO, 3: DEBUG)",
    )

    args = parser.parse_args()

    try:
        # The target directory to be read. This is correct as is.
        root_dir = Path(args.directory).resolve(strict=True)
        if not root_dir.is_dir():
            print(f"Error: The path '{root_dir}' is not a valid directory.", file=sys.stderr)
            sys.exit(1)
    except FileNotFoundError:
        print(f"Error: The directory '{args.directory}' does not exist.", file=sys.stderr)
        sys.exit(1)
    except Exception as e:
        print(f"Error resolving directory path '{args.directory}': {e}", file=sys.stderr)
        sys.exit(1)

    # ---> CHANGE: Resolve output file relative to the current working directory.
    # .resolve() makes the path absolute from CWD if it's relative.
    # If no output file is specified or "-" is used, output_file will be None
    output_file = None
    if args.output_file and args.output_file != "-":
        output_file = Path(args.output_file).resolve()

    ignore_file_path: Optional[Path] = None
    if args.ignore_file:
        # ---> CHANGE: Resolve custom ignore file relative to CWD.
        # We don't use strict=True here because the ignore.py will handle non-existent files with a warning.
        ignore_file_path = Path(args.ignore_file).resolve()

    return (
        root_dir,
        ignore_file_path,
        output_file,
        args.no_default_ignores,
        args.verbosity,
        args.format,
        args.max_depth,
        args.no_content,
        args.max_file_bytes,
    )
