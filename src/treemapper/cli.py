import argparse
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

from .version import __version__

DEFAULT_MAX_FILE_BYTES = 10 * 1024 * 1024  # 10 MB


@dataclass
class ParsedArgs:
    root_dir: Path
    ignore_file: Optional[Path]
    output_file: Optional[Path]
    no_default_ignores: bool
    verbosity: int
    output_format: str
    max_depth: Optional[int]
    no_content: bool
    max_file_bytes: Optional[int]
    copy: bool
    force_stdout: bool


DEFAULT_IGNORES_HELP = """
Default ignored patterns (use --no-default-ignores to include all):
  .git/, .svn/, .hg/    Version control directories
  __pycache__/, *.py[cod], *.so, venv/, .venv/, .tox/, .nox/  Python
  node_modules/, .npm/  JavaScript/Node
  target/, .gradle/     Java/Maven/Gradle
  bin/, obj/            .NET
  vendor/               Go/PHP
  dist/, build/, out/   Generic build output
  .*_cache/             All cache dirs (.pytest_cache, .mypy_cache, etc.)
  .idea/, .vscode/      IDE configurations
  .DS_Store, Thumbs.db  OS-specific files
  tree.{yaml,json,md,txt}  Default output files (auto-ignored)

Ignore files (hierarchical, like git):
  .gitignore            Standard git ignore patterns
  .treemapperignore     TreeMapper-specific patterns
"""


def parse_args() -> ParsedArgs:
    parser = argparse.ArgumentParser(
        prog="treemapper",
        description="Generate a structured representation of a directory tree (YAML, JSON, or text).",
        epilog=DEFAULT_IGNORES_HELP,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )

    parser.add_argument("-v", "--version", action="version", version=f"%(prog)s {__version__}")
    parser.add_argument("directory", nargs="?", default=".", help="The directory to analyze")
    parser.add_argument("-i", "--ignore-file", default=None, help="Path to custom ignore file")
    parser.add_argument(
        "-o",
        "--output-file",
        nargs="?",
        const="",
        default=None,
        help="Output file (default: stdout, use '-' for stdout, omit filename for tree.{ext})",
    )
    parser.add_argument(
        "-f",
        "--format",
        choices=["yaml", "yml", "json", "txt", "md"],
        default="yaml",
        help="Output format (default: yaml)",
    )
    parser.add_argument("--no-default-ignores", action="store_true", help="Disable all default ignores")
    parser.add_argument("--max-depth", type=int, default=None, metavar="N", help="Maximum traversal depth")
    parser.add_argument("--no-content", action="store_true", help="Skip file contents (structure only)")
    parser.add_argument(
        "--max-file-bytes",
        type=int,
        default=DEFAULT_MAX_FILE_BYTES,
        metavar="N",
        help=f"Skip files larger than N bytes (default: {DEFAULT_MAX_FILE_BYTES // 1024 // 1024} MB, use 0 for unlimited)",
    )
    parser.add_argument("-c", "--copy", action="store_true", help="Copy to clipboard (suppresses stdout unless -o is used)")
    parser.add_argument(
        "--log-level",
        choices=["error", "warning", "info", "debug"],
        default="error",
        help="Log level (default: error)",
    )

    args = parser.parse_args()

    if args.max_depth is not None and args.max_depth < 0:
        print(f"Error: --max-depth must be non-negative, got {args.max_depth}", file=sys.stderr)
        sys.exit(1)

    if args.max_depth == 0:
        print("Warning: --max-depth 0 produces empty tree (root only, no children)", file=sys.stderr)

    if args.max_file_bytes < 0:
        print(f"Error: --max-file-bytes must be non-negative, got {args.max_file_bytes}", file=sys.stderr)
        sys.exit(1)

    max_file_bytes = args.max_file_bytes if args.max_file_bytes > 0 else None

    try:
        root_dir = Path(args.directory).resolve(strict=True)
        if not root_dir.is_dir():
            print(f"Error: '{root_dir}' is not a directory.", file=sys.stderr)
            sys.exit(1)
    except FileNotFoundError:
        print(f"Error: Directory '{args.directory}' does not exist.", file=sys.stderr)
        sys.exit(1)
    except OSError as e:
        print(f"Error: Cannot access '{args.directory}': {e}", file=sys.stderr)
        sys.exit(1)

    output_format = "yaml" if args.format == "yml" else args.format

    # Track if -o - was explicitly passed (forces stdout even with --copy)
    force_stdout = args.output_file == "-"

    output_file = None
    if args.output_file is not None and args.output_file != "-":
        if args.output_file == "":
            ext = "yaml" if output_format == "yaml" else output_format
            output_file = Path(f"tree.{ext}").resolve()
        else:
            output_file = Path(args.output_file).resolve()
            if output_file.is_dir():
                print(f"Error: '{args.output_file}' is a directory, not a file.", file=sys.stderr)
                sys.exit(1)

    ignore_file = None
    if args.ignore_file:
        ignore_file = Path(args.ignore_file).resolve()
        if not ignore_file.is_file():
            print(f"Error: Ignore file '{args.ignore_file}' does not exist.", file=sys.stderr)
            sys.exit(1)

    log_level_map = {"error": 0, "warning": 1, "info": 2, "debug": 3}
    verbosity = log_level_map[args.log_level]

    return ParsedArgs(
        root_dir=root_dir,
        ignore_file=ignore_file,
        output_file=output_file,
        no_default_ignores=args.no_default_ignores,
        verbosity=verbosity,
        output_format=output_format,
        max_depth=args.max_depth,
        no_content=args.no_content,
        max_file_bytes=max_file_bytes,
        copy=args.copy,
        force_stdout=force_stdout,
    )
