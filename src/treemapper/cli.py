from __future__ import annotations

import argparse
import sys
from dataclasses import dataclass
from pathlib import Path

from .version import __version__

DEFAULT_MAX_FILE_BYTES = 10 * 1024 * 1024  # 10 MB


def _exit_error(message: str) -> None:
    print(f"Error: {message}", file=sys.stderr)
    sys.exit(1)


def _validate_max_depth(max_depth: int | None) -> None:
    if max_depth is not None and max_depth < 0:
        _exit_error(f"--max-depth must be non-negative, got {max_depth}")
    if max_depth == 0:
        print("Warning: --max-depth 0 produces empty tree (root only, no children)", file=sys.stderr)


def _validate_max_file_bytes(max_file_bytes: int) -> int | None:
    if max_file_bytes < 0:
        _exit_error(f"--max-file-bytes must be non-negative, got {max_file_bytes}")
    return max_file_bytes if max_file_bytes > 0 else None


def _validate_budget(budget: int | None) -> None:
    if budget is not None and budget <= 0:
        _exit_error(f"--budget must be positive, got {budget}")


def _validate_alpha(alpha: float) -> None:
    is_valid = 0 < alpha < 1
    if not is_valid:
        _exit_error(f"--alpha must be between 0 and 1 (exclusive), got {alpha}")


def _validate_tau(tau: float) -> None:
    if tau < 0:
        _exit_error(f"--tau must be non-negative, got {tau}")


def _resolve_root_dir(directory: str) -> Path:
    try:
        root_dir = Path(directory).resolve(strict=True)
        if not root_dir.is_dir():
            _exit_error(f"'{root_dir}' is not a directory.")
        return root_dir
    except FileNotFoundError:
        _exit_error(f"Directory '{directory}' does not exist.")
    except OSError as e:
        _exit_error(f"Cannot access '{directory}': {e}")
    raise RuntimeError("unreachable")


def _resolve_output_file(output_file_arg: str | None, output_format: str) -> tuple[Path | None, bool]:
    if output_file_arg is None:
        return None, False
    if output_file_arg == "-":
        return None, True

    if output_file_arg == "":
        ext = "yaml" if output_format == "yaml" else output_format
        return Path(f"tree.{ext}").resolve(), False

    output_file = Path(output_file_arg).resolve()
    if output_file.is_dir():
        _exit_error(f"'{output_file_arg}' is a directory, not a file.")
    return output_file, False


def _resolve_ignore_file(ignore_file_arg: str | None) -> Path | None:
    if not ignore_file_arg:
        return None
    ignore_file = Path(ignore_file_arg).resolve()
    if not ignore_file.is_file():
        _exit_error(f"Ignore file '{ignore_file_arg}' does not exist.")
    return ignore_file


@dataclass
class ParsedArgs:
    root_dir: Path
    ignore_file: Path | None
    output_file: Path | None
    no_default_ignores: bool
    verbosity: int
    output_format: str
    max_depth: int | None
    no_content: bool
    max_file_bytes: int | None
    copy: bool
    force_stdout: bool
    diff_range: str | None = None
    budget: int | None = None
    alpha: float = 0.55
    tau: float = 0.08
    full_diff: bool = False


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

    diff_group = parser.add_argument_group("diff context mode")
    diff_group.add_argument(
        "--diff",
        dest="diff_range",
        metavar="RANGE",
        help="Git diff range (e.g., HEAD~1..HEAD, main..feature)",
    )
    diff_group.add_argument(
        "--budget",
        type=int,
        default=None,
        metavar="N",
        help="Token budget for diff context output (default: 50000)",
    )
    diff_group.add_argument(
        "--alpha",
        type=float,
        default=0.60,
        metavar="F",
        help="PPR damping factor (default: 0.60)",
    )
    diff_group.add_argument(
        "--tau",
        type=float,
        default=0.08,
        metavar="F",
        help="Stopping threshold for marginal utility (default: 0.08)",
    )
    diff_group.add_argument(
        "--full",
        action="store_true",
        help="Include all changed code (skip smart selection algorithm)",
    )

    args = parser.parse_args()

    _validate_max_depth(args.max_depth)
    max_file_bytes = _validate_max_file_bytes(args.max_file_bytes)
    _validate_budget(args.budget)
    _validate_alpha(args.alpha)
    _validate_tau(args.tau)

    root_dir = _resolve_root_dir(args.directory)
    output_format = "yaml" if args.format == "yml" else args.format
    output_file, force_stdout = _resolve_output_file(args.output_file, output_format)
    ignore_file = _resolve_ignore_file(args.ignore_file)

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
        diff_range=args.diff_range,
        budget=args.budget,
        alpha=args.alpha,
        tau=args.tau,
        full_diff=args.full,
    )
