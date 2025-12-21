import argparse
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

from .version import __version__


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


def parse_args() -> ParsedArgs:
    parser = argparse.ArgumentParser(
        prog="treemapper",
        description="Generate a structured representation of a directory tree (YAML, JSON, or text).",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )

    parser.add_argument("--version", action="version", version=f"%(prog)s {__version__}")
    parser.add_argument("directory", nargs="?", default=".", help="The directory to analyze")
    parser.add_argument("-i", "--ignore-file", default=None, help="Path to custom ignore file")
    parser.add_argument("-o", "--output-file", default=None, help="Output file (default: stdout, use '-' to force stdout)")
    parser.add_argument("--format", choices=["yaml", "json", "text"], default="yaml", help="Output format")
    parser.add_argument("--no-default-ignores", action="store_true", help="Disable all default ignores")
    parser.add_argument("--max-depth", type=int, default=None, metavar="N", help="Maximum traversal depth")
    parser.add_argument("--no-content", action="store_true", help="Skip file contents (structure only)")
    parser.add_argument("--max-file-bytes", type=int, default=None, metavar="N", help="Skip files larger than N bytes")
    parser.add_argument(
        "-v",
        "--verbosity",
        type=int,
        choices=range(0, 4),
        default=0,
        metavar="[0-3]",
        help="Verbosity: 0=ERROR, 1=WARNING, 2=INFO, 3=DEBUG",
    )

    args = parser.parse_args()

    if args.max_depth is not None and args.max_depth < 0:
        print(f"Error: --max-depth must be non-negative, got {args.max_depth}", file=sys.stderr)
        sys.exit(1)

    if args.max_file_bytes is not None and args.max_file_bytes < 0:
        print(f"Error: --max-file-bytes must be non-negative, got {args.max_file_bytes}", file=sys.stderr)
        sys.exit(1)

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

    output_file = None
    if args.output_file and args.output_file != "-":
        output_file = Path(args.output_file).resolve()

    ignore_file = None
    if args.ignore_file:
        ignore_file = Path(args.ignore_file).resolve()
        if not ignore_file.is_file():
            print(f"Error: Ignore file '{args.ignore_file}' does not exist.", file=sys.stderr)
            sys.exit(1)

    return ParsedArgs(
        root_dir=root_dir,
        ignore_file=ignore_file,
        output_file=output_file,
        no_default_ignores=args.no_default_ignores,
        verbosity=args.verbosity,
        output_format=args.format,
        max_depth=args.max_depth,
        no_content=args.no_content,
        max_file_bytes=args.max_file_bytes,
    )
