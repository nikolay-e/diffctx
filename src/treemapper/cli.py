from __future__ import annotations

import argparse
import logging
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import NoReturn

from .version import __version__

logger = logging.getLogger(__name__)

DEFAULT_MAX_FILE_BYTES = 10 * 1024 * 1024  # 10 MB


def _exit_error(message: str) -> NoReturn:
    print(f"Error: {message}", file=sys.stderr)
    sys.exit(1)


def _validate_max_depth(max_depth: int | None) -> None:
    if max_depth is not None and max_depth < 0:
        _exit_error(f"--max-depth must be non-negative, got {max_depth}")
    if max_depth == 0:
        print("Warning: --max-depth 0 produces empty tree (root only, no children)", file=sys.stderr)


def _validate_max_file_bytes(max_file_bytes: int, no_file_size_limit: bool) -> int | None:
    if no_file_size_limit:
        return None
    if max_file_bytes < 0:
        _exit_error(f"--max-file-bytes must be non-negative, got {max_file_bytes}")
    if max_file_bytes == 0:
        _exit_error("--max-file-bytes 0 is ambiguous. Use --no-file-size-limit to include all files regardless of size")
    return max_file_bytes


def _validate_budget(budget: int | None) -> None:
    if budget is not None and budget <= 0:
        _exit_error(f"--budget must be positive, got {budget}")


def _validate_alpha(alpha: float) -> None:
    if not (0 < alpha < 1):
        _exit_error(f"--alpha must be between 0 and 1 (exclusive), got {alpha}")


def _validate_tau(tau: float) -> None:
    if tau < 0:
        _exit_error(f"--tau must be non-negative, got {tau}")


def _resolve_root_dir(directory: str) -> Path:
    try:
        root_dir = Path(directory).resolve(strict=True)
        if not root_dir.is_dir():
            _exit_error(f"'{root_dir}' is not a directory")
        return root_dir
    except FileNotFoundError:
        _exit_error(f"Directory '{directory}' does not exist")
    except OSError as e:
        _exit_error(f"Cannot access '{directory}': {e}")


def _resolve_output_file(output_file_arg: str | None, save: bool, output_format: str) -> tuple[Path | None, bool]:
    if save and output_file_arg is not None:
        _exit_error("--save and -o/--output-file are mutually exclusive")

    if save:
        ext = "yaml" if output_format == "yaml" else output_format
        return Path(f"tree.{ext}").resolve(), False

    if output_file_arg is None:
        return None, False
    if output_file_arg == "-":
        return None, True

    output_file = Path(output_file_arg).resolve()
    if output_file.is_dir():
        _exit_error(f"'{output_file_arg}' is a directory, not a file")
    return output_file, False


def _find_in_treemapper_dir(arg: str, root_dir: Path, extra_exts: tuple[str, ...]) -> Path | None:
    if Path(arg).parent != Path("."):
        return None
    stem = Path(arg).stem if Path(arg).suffix else arg
    base = root_dir / ".treemapper"
    for name in (arg, *(f"{stem}{ext}" for ext in extra_exts if f"{stem}{ext}" != arg)):
        candidate = base / name
        if candidate.is_file():
            return candidate
    return None


def _resolve_ignore_file(ignore_file_arg: str | None, root_dir: Path) -> Path | None:
    if not ignore_file_arg:
        return None
    found = _find_in_treemapper_dir(ignore_file_arg, root_dir, (".ignore", ".txt"))
    if found:
        return found
    resolved = Path(ignore_file_arg).resolve()
    if not resolved.is_file():
        _exit_error(f"Ignore file '{ignore_file_arg}' does not exist")
    return resolved


def _resolve_whitelist_file(whitelist_file_arg: str | None, root_dir: Path) -> Path | None:
    if not whitelist_file_arg:
        return None
    found = _find_in_treemapper_dir(whitelist_file_arg, root_dir, (".whitelist", ".txt"))
    if found:
        return found
    resolved = Path(whitelist_file_arg).resolve()
    if not resolved.is_file():
        logger.warning("Whitelist file '%s' does not exist, skipping", whitelist_file_arg)
        return None
    return resolved


@dataclass
class GraphArgs:
    format: str = "mermaid"
    summary: bool = False
    level: str = "directory"


@dataclass
class ParsedArgs:
    root_dir: Path
    ignore_file: Path | None
    whitelist_file: Path | None
    output_file: Path | None
    no_default_ignores: bool
    verbosity: int | str
    output_format: str
    max_depth: int | None
    no_content: bool
    max_file_bytes: int | None
    copy: bool
    force_stdout: bool
    quiet: bool = False
    diff_range: str | None = None
    budget: int | None = None
    alpha: float = 0.60
    tau: float = 0.08
    full_diff: bool = False
    command: str | None = None
    graph: GraphArgs | None = None


DEFAULT_IGNORES_HELP = """
Default ignored patterns (use --no-default-ignores to disable built-in patterns;
project-level .gitignore and .treemapper/ignore still apply):
  .git/, .svn/, .hg/    Version control directories
  __pycache__/, *.py[cod], *.so, venv/, .venv/, .tox/, .nox/  Python
  node_modules/, .npm/  JavaScript/Node
  package-lock.json, yarn.lock, pnpm-lock.yaml  JS lock files
  Pipfile.lock, poetry.lock, Cargo.lock, Gemfile.lock  Other lock files
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
  .treemapper/ignore    TreeMapper-specific patterns

Whitelist files (auto-discovered):
  .treemapper/whitelist  Include-only filter

Examples:
  treemapper .                    Map current directory to YAML
  treemapper /path/to/project     Map a specific directory
  treemapper . -f json            Output as JSON
  treemapper . -f md --save       Save as tree.md
  treemapper . --diff HEAD~1      Show context for last commit
  treemapper . -c                 Copy output to clipboard
  treemapper . --no-content       Structure only, no file contents

Output routing:
  Default:      stdout
  -o FILE:      write to FILE
  --save:       write to tree.{ext} (e.g., tree.yaml)
  -c:           copy to clipboard, suppress stdout
  -c -o FILE:   copy to clipboard AND write to FILE
"""


def _build_shared_parser() -> argparse.ArgumentParser:
    shared = argparse.ArgumentParser(add_help=False)
    shared.add_argument("-o", "--output-file", default=None, help="Write output to FILE")
    shared.add_argument("-i", "--ignore", default=None, help="Path to custom ignore file")
    shared.add_argument("-w", "--whitelist", default=None, help="Path to whitelist file (only matching files are included)")
    shared.add_argument(
        "--no-default-ignores",
        action="store_true",
        help="Disable built-in ignore patterns (project .gitignore and .treemapper/ignore still apply)",
    )
    shared.add_argument("-c", "--copy", action="store_true", help="Copy to clipboard")
    shared.add_argument("-q", "--quiet", action="store_true", help="Suppress all non-error output")
    shared.add_argument(
        "--log-level",
        choices=["error", "warning", "info", "debug"],
        default="error",
        help="Log level (default: error)",
    )
    return shared


def _build_graph_parser() -> argparse.ArgumentParser:
    graph_parser = argparse.ArgumentParser(
        prog="treemapper graph",
        description="Build and analyze the project dependency graph",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        parents=[_build_shared_parser()],
    )
    graph_parser.add_argument("directory", nargs="?", default=".", help="The directory to analyze")
    graph_parser.add_argument(
        "-f",
        "--format",
        choices=["mermaid", "json", "graphml"],
        default="mermaid",
        help="Graph output format (default: mermaid)",
    )
    graph_parser.add_argument(
        "--summary", action="store_true", help="Print graph statistics (cycles, hotspots, coupling metrics)"
    )
    graph_parser.add_argument(
        "--level",
        choices=["fragment", "file", "directory"],
        default="directory",
        help="Granularity level for graph operations (default: directory)",
    )
    return graph_parser


def _build_main_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="treemapper",
        description=(
            "Generate a structured representation of a directory tree (YAML, JSON, text, or Markdown). "
            "Supports diff context mode (--diff) for intelligent code change analysis.\n\n"
            "Subcommands:\n"
            "  graph    Build and analyze the project dependency graph"
        ),
        epilog=DEFAULT_IGNORES_HELP,
        formatter_class=argparse.RawDescriptionHelpFormatter,
        parents=[_build_shared_parser()],
    )

    parser.add_argument("-v", "--version", action="version", version=f"%(prog)s {__version__}")
    parser.add_argument("directory", nargs="?", default=".", help="The directory to analyze")
    parser.add_argument(
        "-f",
        "--format",
        choices=["yaml", "json", "txt", "md"],
        default="yaml",
        help="Output format (default: yaml)",
    )
    parser.add_argument(
        "--save",
        action="store_true",
        help="Save output to tree.{ext} (e.g., tree.yaml, tree.json)",
    )
    parser.add_argument("--max-depth", type=int, default=None, metavar="N", help="Maximum traversal depth")
    parser.add_argument("--no-content", action="store_true", help="Skip file contents (structure only)")
    parser.add_argument(
        "--max-file-bytes",
        type=int,
        default=DEFAULT_MAX_FILE_BYTES,
        metavar="N",
        help=f"Skip files larger than N bytes (default: {DEFAULT_MAX_FILE_BYTES // 1024 // 1024} MB)",
    )
    parser.add_argument(
        "--no-file-size-limit",
        action="store_true",
        help="Include all files regardless of size",
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
        help="Token budget for context selection (default: automatic via relevance threshold)",
    )
    diff_group.add_argument(
        "--alpha",
        type=float,
        default=0.60,
        metavar="F",
        help="How tightly context clusters around changes, 0-1 (default: 0.60, higher = more focused)",
    )
    diff_group.add_argument(
        "--tau",
        type=float,
        default=0.08,
        metavar="F",
        help="Minimum relevance to include a fragment (default: 0.08, lower = more context)",
    )
    diff_group.add_argument(
        "--full",
        action="store_true",
        help="Include all changed code (skip smart selection algorithm)",
    )
    return parser


def _warn_diff_only_flags(args: argparse.Namespace) -> None:
    if args.diff_range:
        return
    used = []
    if args.budget is not None:
        used.append("--budget")
    if abs(args.alpha - 0.60) > 1e-9:
        used.append("--alpha")
    if abs(args.tau - 0.08) > 1e-9:
        used.append("--tau")
    if args.full:
        used.append("--full")
    if used:
        flags = ", ".join(used)
        print(f"Warning: diff-mode flags ignored without --diff: {flags}", file=sys.stderr)


def _build_graph_parsed_args(args: argparse.Namespace) -> ParsedArgs:
    root_dir = _resolve_root_dir(args.directory)
    output_file_path = Path(args.output_file).resolve() if args.output_file else None
    ignore_file = _resolve_ignore_file(args.ignore, root_dir)
    whitelist_file = _resolve_whitelist_file(args.whitelist, root_dir)
    verbosity = "error" if args.quiet else args.log_level

    return ParsedArgs(
        root_dir=root_dir,
        ignore_file=ignore_file,
        whitelist_file=whitelist_file,
        output_file=output_file_path,
        no_default_ignores=args.no_default_ignores,
        verbosity=verbosity,
        output_format="yaml",
        max_depth=None,
        no_content=False,
        max_file_bytes=None,
        copy=args.copy,
        force_stdout=False,
        quiet=args.quiet,
        command="graph",
        graph=GraphArgs(
            format=args.format,
            summary=args.summary,
            level=args.level,
        ),
    )


def _build_tree_parsed_args(args: argparse.Namespace) -> ParsedArgs:
    _validate_max_depth(args.max_depth)
    max_file_bytes = _validate_max_file_bytes(args.max_file_bytes, args.no_file_size_limit)
    _validate_budget(args.budget)
    _validate_alpha(args.alpha)
    _validate_tau(args.tau)
    _warn_diff_only_flags(args)

    root_dir = _resolve_root_dir(args.directory)
    output_format = args.format
    output_file, force_stdout = _resolve_output_file(args.output_file, args.save, output_format)
    ignore_file = _resolve_ignore_file(args.ignore, root_dir)
    whitelist_file = _resolve_whitelist_file(args.whitelist, root_dir)
    verbosity = "error" if args.quiet else args.log_level

    return ParsedArgs(
        root_dir=root_dir,
        ignore_file=ignore_file,
        whitelist_file=whitelist_file,
        output_file=output_file,
        no_default_ignores=args.no_default_ignores,
        verbosity=verbosity,
        output_format=output_format,
        max_depth=args.max_depth,
        no_content=args.no_content,
        max_file_bytes=max_file_bytes,
        copy=args.copy,
        force_stdout=force_stdout,
        quiet=args.quiet,
        diff_range=args.diff_range,
        budget=args.budget,
        alpha=args.alpha,
        tau=args.tau,
        full_diff=args.full,
    )


def parse_args(argv: list[str] | None = None) -> ParsedArgs:
    raw_args = sys.argv[1:] if argv is None else argv

    if raw_args and raw_args[0] == "graph":
        args = _build_graph_parser().parse_args(raw_args[1:])
        return _build_graph_parsed_args(args)

    args = _build_main_parser().parse_args(raw_args)
    return _build_tree_parsed_args(args)
