from __future__ import annotations

import argparse
import logging
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import NoReturn

from diffctx._diffctx import (
    DEFAULT_ALPHA as _ENGINE_DEFAULT_ALPHA,
)
from diffctx._diffctx import (
    DEFAULT_SCORING as _ENGINE_DEFAULT_SCORING,
)
from diffctx._diffctx import (
    DEFAULT_TAU as _ENGINE_DEFAULT_TAU,
)
from diffctx._diffctx import (
    SCORING_MODES as _ENGINE_SCORING_MODES,
)

from .version import __version__

logger = logging.getLogger(__name__)

DEFAULT_MAX_FILE_BYTES = 256 * 1024  # 256 KB
# Read from the engine, not restated. These same three numbers were written out
# by hand in the CLI, the MCP server and two eval harnesses, and they drifted:
# tau shipped as 0.12 while contextbench measured 0.08 and the in-memory runner
# 0.05 (#175). The extension is the one place that cannot disagree with itself.
_DEFAULT_ALPHA: float = _ENGINE_DEFAULT_ALPHA
_DEFAULT_TAU: float = _ENGINE_DEFAULT_TAU
# The accepted --scoring values, for the same reason: this list was a literal
# and went stale the moment a mode was added to the engine.
_SCORING_CHOICES: list[str] = list(_ENGINE_SCORING_MODES)
_DEFAULT_SCORING: str = _ENGINE_DEFAULT_SCORING
# Mirrors DEFAULT_PIPELINE_TIMEOUT_SECONDS in crates/diffctx-native/src/config/limits.rs.
_DEFAULT_TIMEOUT = 300


class _Unset:
    def __repr__(self) -> str:
        return "<unset>"


_UNSET: _Unset = _Unset()
_DIFF_SENTINEL = "__DIFFCTX_DIFF_BARE__"

# Set once at the top of parse_args(); read by _exit_error/_warn so every
# CLI-side validation message is branded like argparse's own native errors
# (`{prog}: error: ...`) instead of a bare, unbranded "Error: ...". Module
# state (rather than threading `prog` through every _validate_* signature)
# keeps this a same-session fix; see #89.
_current_prog: str = "diffctx"


def _exit_error(message: str) -> NoReturn:
    print(f"{_current_prog}: error: {message}", file=sys.stderr)
    sys.exit(1)


def _exit_usage_error(message: str) -> NoReturn:
    print(f"{_current_prog}: error: {message}", file=sys.stderr)
    sys.exit(2)


def _warn(message: str) -> None:
    print(f"{_current_prog}: warning: {message}", file=sys.stderr)


def _validate_max_depth(max_depth: int | None) -> None:
    if max_depth is not None and max_depth < 0:
        _exit_usage_error(f"--max-depth must be non-negative, got {max_depth}")
    if max_depth == 0:
        _warn("--max-depth 0 produces empty tree (root only, no children)")


def _validate_max_file_bytes(max_file_bytes: int, no_file_size_limit: bool) -> int | None:
    if no_file_size_limit:
        return None
    if max_file_bytes < 0:
        _exit_usage_error(f"--max-file-bytes must be non-negative, got {max_file_bytes}")
    if max_file_bytes == 0:
        _exit_usage_error("--max-file-bytes 0 is ambiguous. Use --no-file-size-limit to include all files regardless of size")
    return max_file_bytes


def _validate_budget(budget: int | None) -> None:
    if budget is not None and budget < -1:
        _exit_usage_error(
            f"--budget must be >= -1 (-1 = unlimited, 0 = strict-zero floor; use --full for changed files only), got {budget}"
        )


def _validate_timeout(timeout: int) -> None:
    if timeout < 1:
        _exit_usage_error(f"--timeout must be >= 1 second, got {timeout}")


def _validate_alpha(alpha: float) -> None:
    if not (0 < alpha < 1):
        _exit_usage_error(f"--alpha must be between 0 and 1 (exclusive), got {alpha}")


def _validate_tau(tau: float) -> None:
    if tau < 0:
        _exit_usage_error(f"--tau must be non-negative, got {tau}")


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


def _no_match_error(pattern: str) -> NoReturn:
    if pattern == "graph":
        _exit_error("No matches for 'graph'; if you meant the subcommand, it must come first: diffctx graph [options]")
    if "*" in pattern and "**" not in pattern:
        _exit_error(f"No matches for '{pattern}' (globs are not recursive; try '**/{pattern}')")
    _exit_error(f"No matches for '{pattern}'")


def _resolve_glob_pattern(pattern: str) -> list[str]:
    import glob as globmod

    matches = sorted(globmod.glob(pattern, recursive=True))
    if matches:
        return matches
    try:
        p = Path(pattern).resolve(strict=True)
    except FileNotFoundError:
        _no_match_error(pattern)
    except OSError as e:
        _exit_error(f"Cannot access '{pattern}': {e}")
    return [str(p)]


def _classify_resolved(resolved: Path, dirs: list[Path], files: list[Path]) -> None:
    if resolved.is_dir():
        dirs.append(resolved)
    elif resolved.is_file():
        files.append(resolved)


def _expand_paths(raw_paths: list[str]) -> tuple[list[Path], list[Path]]:
    dirs: list[Path] = []
    files: list[Path] = []
    seen: set[Path] = set()
    for pattern in raw_paths:
        for m in _resolve_glob_pattern(pattern):
            try:
                resolved = Path(m).resolve()
            except OSError as e:
                _exit_error(f"Cannot access '{m}': {e}")
            if resolved in seen:
                continue
            seen.add(resolved)
            _classify_resolved(resolved, dirs, files)
    return dirs, files


_FORMAT_BY_EXTENSION = {
    ".yaml": "yaml",
    ".yml": "yaml",
    ".json": "json",
    ".md": "md",
    ".markdown": "md",
    ".txt": "txt",
}


def _infer_format_from_output_file(output_file_arg: str | None) -> str | None:
    if not output_file_arg or output_file_arg == "-":
        return None
    return _FORMAT_BY_EXTENSION.get(Path(output_file_arg).suffix.lower())


def _resolve_format(format_arg: str | _Unset, output_file_arg: str | None) -> str:
    inferred = _infer_format_from_output_file(output_file_arg)
    if isinstance(format_arg, str):
        if inferred and inferred != format_arg:
            _warn(f"-f {format_arg} does not match the '{output_file_arg}' extension; writing {format_arg}")
        return format_arg
    return inferred or "md"


def _resolve_output_file(output_file_arg: str | None, save: bool, output_format: str) -> tuple[Path | None, bool]:
    if save and output_file_arg is not None:
        _exit_usage_error("--save and -o/--output-file are mutually exclusive")

    if save:
        ext = "yaml" if output_format == "yaml" else output_format
        return Path(f"tree.{ext}").resolve(), False

    if output_file_arg is None:
        return None, False
    if output_file_arg == "-":
        return None, True

    output_file = Path(output_file_arg).resolve()
    if output_file.is_dir():
        _exit_usage_error(f"'{output_file_arg}' is a directory, not a file")
    return output_file, False


def _find_in_diffctx_dir(arg: str, root_dir: Path, extra_exts: tuple[str, ...]) -> Path | None:
    if Path(arg).parent != Path("."):
        return None
    stem = Path(arg).stem if Path(arg).suffix else arg
    base = root_dir / ".diffctx"
    for name in (arg, *(f"{stem}{ext}" for ext in extra_exts if f"{stem}{ext}" != arg)):
        candidate = base / name
        if candidate.is_file():
            return candidate
    return None


def _resolve_config_file(file_arg: str | None, root_dir: Path, extensions: tuple[str, ...], label: str) -> Path | None:
    if not file_arg:
        return None
    found = _find_in_diffctx_dir(file_arg, root_dir, extensions)
    if found:
        return found
    resolved = Path(file_arg).resolve()
    if not resolved.is_file():
        _exit_error(f"{label} file '{file_arg}' does not exist")
    return resolved


def _resolve_ignore_file(ignore_file_arg: str | None, root_dir: Path) -> Path | None:
    return _resolve_config_file(ignore_file_arg, root_dir, (".ignore", ".txt"), "Ignore")


def _resolve_whitelist_file(whitelist_file_arg: str | None, root_dir: Path) -> Path | None:
    return _resolve_config_file(whitelist_file_arg, root_dir, (".whitelist", ".txt"), "Whitelist")


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
    no_ignores: bool = False
    diff_range: str | None = None
    budget: int | None = None
    alpha: float = _DEFAULT_ALPHA
    tau: float = _DEFAULT_TAU
    scoring: str = _DEFAULT_SCORING
    timeout: int = _DEFAULT_TIMEOUT
    full_diff: bool = False
    with_raw_diff: bool = False
    mode: str = "pack"
    command: str | None = None
    graph: GraphArgs | None = None
    extra_dirs: list[Path] | None = None
    extra_files: list[Path] | None = None
    no_explicit_paths: bool = False


def main() -> None:
    """Run the user-facing CLI.

    Argument parsing lives in this module; orchestration remains an internal
    application service so importing ``diffctx.cli`` stays lightweight.
    """
    if sys.argv[1:2] == ["mcp"]:
        _run_mcp_server()
        return

    from ._app import run

    run()


def _run_mcp_server() -> None:
    # MCP clients that resolve the executable from the published package name
    # land on this entry point rather than diffctx-mcp; without this branch they
    # would start a tree-mapping run and flood the transport with the whole
    # working directory instead of speaking the protocol.
    if Path("mcp").is_dir():
        _warn("'mcp' is the subcommand; to map the directory ./mcp, run: diffctx ./mcp")

    try:
        from .mcp.server import main as mcp_main
    except ImportError:
        _fail_missing_mcp_extra()

    sys.argv = [f"{_current_prog} mcp", *sys.argv[2:]]
    mcp_main(prog=f"{_current_prog} mcp")


def _fail_missing_mcp_extra() -> NoReturn:
    print(
        f"{_current_prog}: error: the mcp subcommand requires the 'mcp' extra: pip install 'diffctx[mcp]'",
        file=sys.stderr,
    )
    sys.exit(3)


DEFAULT_IGNORES_HELP = """
Built-in ignored patterns (disable with --no-default-ignores; project .gitignore
and .diffctx/ignore always apply unless --no-ignores is given):
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
  .diffctx/ignore       diffctx-specific patterns

Whitelist files (auto-discovered):
  .diffctx/whitelist    Include-only filter

Examples:
  diffctx .                    Map current directory to Markdown
  diffctx /path/to/project     Map a specific directory
  diffctx . -f json            Output as JSON
  diffctx . --save             Save as tree.md
  diffctx . --diff             Context for uncommitted changes
  diffctx . --diff HEAD~1      Context for the last commit
  diffctx . --diff HEAD~1 --with-raw-diff   Raw patch + selected context in one file
  diffctx . -c                 Copy output to clipboard
  diffctx . --no-content       Structure only, no file contents
  diffctx graph .              Build the project dependency graph (see: diffctx graph --help)
  diffctx graph . --summary    Print graph stats (cycles, hotspots, coupling)

Output routing:
  Default:      stdout
  -o FILE:      write to FILE (format inferred from extension unless -f is given)
  -o -:         force stdout
  --save:       write to tree.{ext} (tree.md by default; extension follows -f)
  -c:           copy to clipboard, suppress stdout
  -c -o FILE:   copy to clipboard AND write to FILE

Token counting (--budget, and the summary line on stderr):
  Every count comes from tiktoken's o200k_base encoder (the GPT-4o/GPT-4.1
  family). It is exact for those models only. Claude, Gemini, Llama and other
  families use different tokenizers, so their counts differ from the number
  printed here — usually by single-digit to low-double-digit percent, in either
  direction. Treat --budget as an upper bound in o200k tokens and leave
  headroom (e.g. --budget 28000 for a 32k target) when the consumer is not an
  OpenAI model. There is no --tokenizer flag; o200k_base is pinned so results
  stay reproducible against the published evaluation.
  --with-raw-diff output is NOT charged to --budget, but IS included in the
  stderr token summary, which always reports the real size of what was written.

Exit codes:
  0  success
  1  runtime error (unreadable path, write failure)
  2  usage error (unknown flag or invalid value)
  3  environment error (git missing, not a repository, unknown revision)
  4  --diff produced no context (clean tree or empty range)
  124  --diff exceeded the --timeout wall-clock deadline
"""


def _build_shared_parser() -> argparse.ArgumentParser:
    shared = argparse.ArgumentParser(add_help=False)
    shared.add_argument(
        "-o", "--output-file", default=None, metavar="FILE", help="Write output to FILE instead of stdout ('-' forces stdout)"
    )
    shared.add_argument(
        "-i",
        "--ignore",
        default=None,
        metavar="FILE",
        help="Custom ignore file (bare names also resolve inside .diffctx/; not yet supported with --diff)",
    )
    shared.add_argument(
        "-w",
        "--whitelist",
        default=None,
        metavar="FILE",
        help="Whitelist file, only matching files are included (bare names also resolve inside .diffctx/; not yet supported with --diff)",
    )
    shared.add_argument(
        "--no-default-ignores",
        action="store_true",
        help="Disable built-in ignore patterns only; project .gitignore and .diffctx/ignore still apply (see --no-ignores)",
    )
    shared.add_argument(
        "-c",
        "--copy",
        action="store_true",
        help="Copy to clipboard instead of printing to stdout (combine with -o to also write a file)",
    )
    shared.add_argument(
        "-q",
        "--quiet",
        action="store_true",
        help="Suppress status messages (token summary, save/copy confirmations); overrides --log-level",
    )
    shared.add_argument(
        "--log-level",
        choices=["error", "warning", "info", "debug"],
        default="error",
        help="Log level (default: error)",
    )
    return shared


def _build_graph_parser(prog: str = "diffctx graph") -> argparse.ArgumentParser:
    graph_parser = argparse.ArgumentParser(
        prog=prog,
        description="Build and analyze the project dependency graph",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        parents=[_build_shared_parser()],
    )
    graph_parser.add_argument("directory", nargs="?", default=".", help="The directory to analyze")
    graph_parser.add_argument(
        "-f",
        "--format",
        choices=["mermaid", "json", "graphml"],
        default=_UNSET,
        help="Graph output format (default: mermaid)",
    )
    graph_parser.add_argument(
        "--summary",
        action="store_true",
        help="Print graph statistics instead of the graph (cycles, hotspots, coupling); -f is ignored",
    )
    graph_parser.add_argument(
        "--level",
        choices=["fragment", "file", "directory"],
        default=_UNSET,
        help="Node granularity: directory, file, or fragment = function/class-level block (default: directory); applies to mermaid output and --summary",
    )
    return graph_parser


def _build_main_parser(prog: str = "diffctx", version: str = __version__) -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog=prog,
        description=(
            "Generate a structured representation of a directory tree (Markdown, YAML, JSON, or text). "
            "Supports diff context mode (--diff) for intelligent code change analysis.\n\n"
            "Subcommands:\n"
            "  graph    Build and analyze the project dependency graph\n"
            "  mcp      Run the MCP server over stdio (same as diffctx-mcp; needs the [mcp] extra)"
        ),
        epilog=DEFAULT_IGNORES_HELP,
        formatter_class=argparse.RawDescriptionHelpFormatter,
        parents=[_build_shared_parser()],
    )

    parser.add_argument("-v", "--version", action="version", version=f"%(prog)s {version}")
    parser.add_argument("paths", nargs="*", default=[], help="Directories, files, or glob patterns to analyze")
    parser.add_argument(
        "-f",
        "--format",
        choices=["yaml", "json", "txt", "md"],
        default=_UNSET,
        help="Output format (default: md; inferred from the -o FILE extension when omitted)",
    )
    parser.add_argument(
        "--save",
        action="store_true",
        help="Save output to tree.{ext} in the current directory (tree.md by default; extension follows -f)",
    )
    parser.add_argument(
        "--no-ignores",
        action="store_true",
        help="Disable all ignore rules: built-in patterns, project .gitignore, and .diffctx/ignore (a custom -i file still applies)",
    )
    parser.add_argument("--max-depth", type=int, default=None, metavar="N", help="Maximum traversal depth (default: unlimited)")
    parser.add_argument("--no-content", action="store_true", help="Skip file contents (structure only)")
    parser.add_argument(
        "--max-file-bytes",
        type=int,
        default=_UNSET,
        metavar="N",
        help=f"Omit content of files larger than N bytes (default: {DEFAULT_MAX_FILE_BYTES // 1024} KB). Use --no-file-size-limit to include all.",
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
        nargs="?",
        const=_DIFF_SENTINEL,
        default=None,
        metavar="RANGE",
        help="Git diff range (e.g., HEAD~1..HEAD, main..feature). Bare --diff shows uncommitted changes (working tree vs HEAD).",
    )
    diff_group.add_argument(
        "--budget",
        type=int,
        default=_UNSET,
        metavar="TOKENS",
        help=(
            "Token budget in o200k_base tokens (tiktoken, GPT-4o family — other model families "
            "tokenize differently, so leave headroom; see 'Token counting' below): "
            "omit = auto (default), N = fixed cap, -1 = unlimited, "
            "0 = strict-zero floor (empty selection; use --full for changed files only)"
        ),
    )
    diff_group.add_argument(
        "--alpha",
        type=float,
        default=_UNSET,
        metavar="FLOAT",
        help=(
            "PPR damping: how tightly context clusters around changes, 0-1 exclusive "
            "(default: 0.60, higher = more focused). Only affects --scoring ppr"
        ),
    )
    diff_group.add_argument(
        "--tau",
        type=float,
        default=_UNSET,
        metavar="FLOAT",
        help=(
            "Relevance threshold for full fragment content, >= 0 (default: 0.12). "
            "Fragments scoring below it are reduced to signature stubs or dropped; "
            "higher = leaner output, lower = more surrounding context"
        ),
    )
    diff_group.add_argument(
        "--scoring",
        choices=_SCORING_CHOICES,
        default=_UNSET,
        help=(
            "Scoring mode: ego = structural neighbors of the change (default); "
            "ppr = graph-wide relevance (Personalized PageRank), for far-reaching changes; "
            "bm25 = lexical similarity, for sparse cross-file structure; "
            "rrf = rank fusion of ego and bm25 on ranks; "
            "pit = the same fusion on score percentiles rather than ranks"
        ),
    )
    diff_group.add_argument(
        "--mode",
        choices=["pack", "locate"],
        default=_UNSET,
        help=(
            "Output mode: pack = context with source bodies (default); "
            "locate = ranked navigation list with provenance reasons, JSON only "
            "(diffctx.locate.v1; -f is ignored)"
        ),
    )
    diff_group.add_argument(
        "--timeout",
        type=int,
        default=_UNSET,
        metavar="SECONDS",
        help=(
            f"Wall-clock deadline for --diff analysis (default: {_DEFAULT_TIMEOUT}); "
            "on expiry diffctx aborts with exit code 124 instead of hanging"
        ),
    )
    diff_group.add_argument(
        "--full",
        action="store_true",
        default=False,
        help="Include every fragment of the changed files and nothing else — no related-code context (ignores --budget/--tau/--alpha/--scoring)",
    )
    diff_group.add_argument(
        "--with-raw-diff",
        action="store_true",
        default=False,
        help=(
            "Also embed the raw unified diff (git's own +/- text) ahead of the selected fragments. "
            "Additive only: selection is unchanged, and the diff does NOT count against --budget "
            "(the stderr token summary counts it, reporting the real output size). "
            "Lock-file, ignored, and secret-like sections stay omitted. Python CLI only — the native binary has no such flag"
        ),
    )
    return parser


def _warn_diff_only_flags(args: argparse.Namespace) -> None:
    if args.diff_range:
        return
    used = []
    if args.budget is not _UNSET:
        used.append("--budget")
    if args.alpha is not _UNSET:
        used.append("--alpha")
    if args.tau is not _UNSET:
        used.append("--tau")
    if args.full:
        used.append("--full")
    if args.with_raw_diff:
        used.append("--with-raw-diff")
    if args.scoring is not _UNSET:
        used.append("--scoring")
    if args.mode is not _UNSET:
        used.append("--mode")
    if args.timeout is not _UNSET:
        used.append("--timeout")
    if used:
        flags = ", ".join(used)
        _warn(f"diff-mode flags ignored without --diff: {flags}")


def _warn_quiet_log_level_conflict(args: argparse.Namespace) -> None:
    if args.quiet and args.log_level != "error":
        _warn(f"--log-level {args.log_level} ignored with -q")


def _build_graph_parsed_args(args: argparse.Namespace) -> ParsedArgs:
    root_dir = _resolve_root_dir(args.directory)
    graph_format = "mermaid" if args.format is _UNSET else args.format
    graph_level = "directory" if args.level is _UNSET else args.level
    if args.summary and args.format is not _UNSET:
        _warn(f"-f {graph_format} ignored with --summary")
    if not args.summary and args.level is not _UNSET and graph_format in ("json", "graphml"):
        _warn(f"--level {graph_level} applies to mermaid output and --summary; ignored for -f {graph_format}")
    _warn_quiet_log_level_conflict(args)
    output_file_path, force_stdout = _resolve_output_file(args.output_file, False, graph_format)
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
        force_stdout=force_stdout,
        quiet=args.quiet,
        command="graph",
        graph=GraphArgs(
            format=graph_format,
            summary=args.summary,
            level=graph_level,
        ),
    )


def _warn_full_selection_conflict(args: argparse.Namespace) -> None:
    if not (args.diff_range and args.full):
        return
    ignored = [
        name
        for name, value in (("--budget", args.budget), ("--alpha", args.alpha), ("--tau", args.tau), ("--scoring", args.scoring))
        if value is not _UNSET
    ]
    if ignored:
        _warn(f"selection flags ignored with --full: {', '.join(ignored)}")


def _resolve_max_file_bytes(args: argparse.Namespace) -> int | None:
    explicit = args.max_file_bytes is not _UNSET
    value = DEFAULT_MAX_FILE_BYTES if not explicit else args.max_file_bytes
    if explicit and args.no_file_size_limit:
        _warn("--max-file-bytes ignored with --no-file-size-limit")
    if explicit and args.no_content:
        _warn("--max-file-bytes has no effect with --no-content")
    return _validate_max_file_bytes(value, args.no_file_size_limit)


def _resolve_diff_params(args: argparse.Namespace) -> tuple[str | None, int | None, float, float, str, int, str]:
    budget = None if args.budget is _UNSET else args.budget
    alpha = _DEFAULT_ALPHA if args.alpha is _UNSET else args.alpha
    tau = _DEFAULT_TAU if args.tau is _UNSET else args.tau
    scoring = _DEFAULT_SCORING if args.scoring is _UNSET else args.scoring
    timeout = _DEFAULT_TIMEOUT if args.timeout is _UNSET else args.timeout
    mode = "pack" if args.mode is _UNSET else args.mode

    _validate_budget(budget)
    _validate_alpha(alpha)
    _validate_tau(tau)
    _validate_timeout(timeout)
    _warn_diff_only_flags(args)
    _warn_full_selection_conflict(args)
    if args.diff_range and not args.full and args.alpha is not _UNSET and scoring != "ppr":
        _warn(f"--alpha only affects --scoring ppr (current scoring: {scoring}); value ignored")

    diff_range = args.diff_range
    if diff_range == _DIFF_SENTINEL:
        diff_range = "HEAD"
    if diff_range and args.no_ignores:
        _exit_usage_error("--no-ignores is not supported with --diff (git's own ignore rules always apply in diff mode)")
    _validate_locate_mode(args, mode)
    return diff_range, budget, alpha, tau, scoring, timeout, mode


def _validate_locate_mode(args: argparse.Namespace, mode: str) -> None:
    if mode != "locate":
        return
    if args.full:
        _exit_usage_error("--mode locate is incompatible with --full (locate ranks the selection; --full bypasses it)")
    if args.with_raw_diff:
        _exit_usage_error("--mode locate emits no source; --with-raw-diff applies to pack mode only")
    if args.format is not _UNSET:
        _warn(f"-f {args.format} ignored with --mode locate (locate emits diffctx.locate.v1 JSON)")


def _build_tree_parsed_args(args: argparse.Namespace) -> ParsedArgs:
    _validate_max_depth(args.max_depth)
    max_file_bytes = _resolve_max_file_bytes(args)
    diff_range, budget, alpha, tau, scoring, timeout, mode = _resolve_diff_params(args)
    _warn_quiet_log_level_conflict(args)

    dirs, files = _expand_paths(args.paths)
    root_dir = dirs[0] if dirs else Path(".").resolve()
    extra_dirs = dirs or None
    extra_files = files or None

    output_format = _resolve_format(args.format, args.output_file)
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
        no_ignores=args.no_ignores,
        diff_range=diff_range,
        budget=budget,
        alpha=alpha,
        tau=tau,
        scoring=scoring,
        timeout=timeout,
        full_diff=args.full,
        with_raw_diff=args.with_raw_diff,
        mode=mode,
        extra_dirs=extra_dirs,
        extra_files=extra_files,
        no_explicit_paths=not args.paths,
    )


def parse_args(argv: list[str] | None = None, *, prog: str = "diffctx", version: str = __version__) -> ParsedArgs:
    global _current_prog
    _current_prog = prog
    raw_args = sys.argv[1:] if argv is None else argv

    if raw_args and raw_args[0] == "graph":
        if Path("graph").is_dir():
            _warn("'graph' is the subcommand; to map the directory ./graph, run: diffctx ./graph")
        args = _build_graph_parser(prog=f"{prog} graph").parse_args(raw_args[1:])
        return _build_graph_parsed_args(args)

    args = _build_main_parser(prog=prog, version=version).parse_args(raw_args)
    return _build_tree_parsed_args(args)
