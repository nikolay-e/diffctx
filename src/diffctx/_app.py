from __future__ import annotations

import argparse
import logging
import os
import re
import signal
import subprocess
import sys
from pathlib import Path
from typing import TYPE_CHECKING, Any, TypeVar

if TYPE_CHECKING:
    from collections.abc import Callable

from .version import __version__

if TYPE_CHECKING:
    from .cli import ParsedArgs

_T = TypeVar("_T")

logger = logging.getLogger(__name__)

_EXIT_RUNTIME = 1
_EXIT_USAGE = 2
_EXIT_ENVIRONMENT = 3
_EXIT_EMPTY_DIFF = 4
_EXIT_TIMEOUT = 124
_EXIT_INTERRUPTED = 130
_EXIT_BROKEN_PIPE = 141


def _configure_windows_utf8() -> None:
    reconfigure = getattr(sys.stdout, "reconfigure", None)
    if reconfigure is None:
        return
    try:
        reconfigure(encoding="utf-8")
    except (AttributeError, ValueError, OSError):
        pass


def _ensure_git_repo(root_dir: Path, prog: str) -> None:
    try:
        result = subprocess.run(
            ["git", "rev-parse", "--git-dir"],
            cwd=str(root_dir),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
    except OSError as exc:
        print(
            f"{prog}: --diff requires git to be installed and on PATH ({exc}); install git or run without --diff.",
            file=sys.stderr,
        )
        sys.exit(_EXIT_ENVIRONMENT)
    if result.returncode != 0:
        print(
            f"{prog}: --diff requires a git repository (cwd: {root_dir}); "
            "run inside a working tree or pass --diff <range> with a valid git context.",
            file=sys.stderr,
        )
        sys.exit(_EXIT_ENVIRONMENT)

    head_result = subprocess.run(
        ["git", "rev-parse", "--verify", "-q", "HEAD"],
        cwd=str(root_dir),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    if head_result.returncode != 0:
        print(
            f"{prog}: --diff requires at least one commit (no HEAD in this repository yet); "
            "commit first, or run without --diff to map the working tree.",
            file=sys.stderr,
        )
        sys.exit(_EXIT_ENVIRONMENT)


def _diff_result_is_empty(result: dict[str, Any]) -> bool:
    if result.get("deleted_files") or result.get("renamed_files") or result.get("lockfile_changes"):
        return False
    # A bundled patch is actionable output on its own; exiting 4 next to a
    # complete diff fails any `set -e` CI step for a run that produced content.
    if result.get("raw_diff"):
        return False
    count = result.get("fragment_count")
    if isinstance(count, int):
        return count == 0
    fragments = result.get("fragments")
    if isinstance(fragments, list):
        return len(fragments) == 0
    return False


def _empty_diff_hint(args: ParsedArgs) -> str:
    if args.budget == 0:
        return "--budget 0 selects only the changed code itself; omit --budget for auto sizing"
    if args.budget is not None and args.budget > 0:
        return f"--budget {args.budget} may be too small to fit any fragment; raise it or omit for auto sizing"
    if args.diff_range == "HEAD":
        return "the working tree matches HEAD; try --diff HEAD~1 for the last commit"
    return f"check the range with: git diff --stat {args.diff_range}"


def _warn_empty_diff_result(result: dict[str, Any], prog: str, args: ParsedArgs) -> None:
    if _diff_result_is_empty(result):
        print(
            f"{prog}: diff produced no semantic context "
            f"(clean working tree, binary-only, or files over the size cap); {_empty_diff_hint(args)}",
            file=sys.stderr,
        )


def _report_raw_diff_share(result: dict[str, Any], prog: str, args: ParsedArgs) -> None:
    if not args.with_raw_diff:
        return
    from .tokens import count_tokens

    raw_diff = result.get("raw_diff")
    if not isinstance(raw_diff, str) or not raw_diff:
        print(
            f"{prog}: --with-raw-diff produced no patch text "
            "(the range only touches untracked, lock, ignored, or secret-like files)",
            file=sys.stderr,
        )
        return
    print(
        f"  of which {count_tokens(raw_diff).count:,} tokens are the raw diff (not charged to --budget)",
        file=sys.stderr,
    )


def _call_with_wall_clock_deadline(build: Callable[[], _T], timeout_seconds: int, prog: str) -> _T:
    # The Rust extension releases the GIL but offers no cancellation, so a
    # pathological repo can hang far past any per-phase git timeout (#70).
    # Mirror the standalone binary's watchdog: run the pipeline on a worker
    # thread and hard-exit 124 on deadline — the runaway worker cannot be
    # stopped, so finalizers must not run (os._exit, not sys.exit).
    import threading

    outcome: list[Any] = []

    def worker() -> None:
        try:
            outcome.append(("ok", build()))
        except Exception as exc:  # KeyboardInterrupt/SystemExit stay on the main thread
            outcome.append(("err", exc))

    thread = threading.Thread(target=worker, name="diffctx-pipeline", daemon=True)
    thread.start()
    thread.join(timeout_seconds)
    if thread.is_alive():
        print(
            f"{prog}: pipeline exceeded {timeout_seconds}s wall-clock deadline; aborting before "
            "OOM/SIGKILL. Narrow the review with an explicit '--diff <from>..<to>' range, "
            "run on a smaller subtree, or raise '--timeout'.",
            file=sys.stderr,
        )
        sys.stderr.flush()
        sys.stdout.flush()
        os._exit(_EXIT_TIMEOUT)
    if not outcome:
        raise RuntimeError("pipeline worker terminated unexpectedly")
    status, value = outcome[0]
    if status == "err":
        raise value
    return value  # type: ignore[no-any-return]


def _build_diff_tree(args: ParsedArgs, prog: str) -> dict[str, Any]:
    from ._native import build_diff_context

    if not args.diff_range:
        raise RuntimeError("diff_range is required in diff mode")
    _ensure_git_repo(args.root_dir, prog)
    result = _call_with_wall_clock_deadline(
        lambda: build_diff_context(
            root_dir=args.root_dir,
            diff_range=args.diff_range or "HEAD",
            budget_tokens=args.budget,
            alpha=args.alpha,
            tau=args.tau,
            no_content=args.no_content,
            ignore_file=args.ignore_file,
            no_default_ignores=args.no_default_ignores,
            full=args.full_diff,
            whitelist_file=args.whitelist_file,
            scoring_mode=args.scoring,
            timeout=args.timeout,
            with_raw_diff=args.with_raw_diff,
        ),
        args.timeout,
        prog,
    )
    _warn_empty_diff_result(result, prog, args)
    return result


def _root_display_name(root_dir: Any) -> str:
    name = root_dir.name
    return name if name else str(root_dir)


_LARGE_OUTPUT_WARN_BYTES = 10 * 1024 * 1024
# A bare `diffctx` with no path argument at all is the most common "just try
# it" first invocation — and the easiest to run somewhere unintended (e.g.
# /tmp, $HOME). Use a much lower bar for that specific case; an explicit path
# argument is a stronger signal the user knows what they're pointing at (#87).
_LARGE_OUTPUT_WARN_BYTES_NO_EXPLICIT_PATH = 1 * 1024 * 1024


def _warn_if_output_oversized(output_content: str, args: ParsedArgs) -> None:
    if args.no_content or args.diff_range:
        return
    threshold = _LARGE_OUTPUT_WARN_BYTES_NO_EXPLICIT_PATH if args.no_explicit_paths else _LARGE_OUTPUT_WARN_BYTES
    size_bytes = len(output_content.encode("utf-8"))
    if size_bytes < threshold:
        return
    mb = size_bytes / (1024 * 1024)
    print(
        f"Warning: output is {mb:.1f} MB. For large repos try --no-content (structure only), "
        f"--diff RANGE (relevance-ranked context), or --save / -o FILE to keep it off the terminal.",
        file=sys.stderr,
    )


def _build_file_node(file_path: Path, base_dir: Path, no_content: bool, max_file_bytes: int | None) -> dict[str, Any]:
    from .tree import _read_file_content

    try:
        rel = file_path.relative_to(base_dir).as_posix()
    except ValueError:
        try:
            rel = file_path.relative_to(Path.cwd()).as_posix()
        except ValueError:
            rel = file_path.name
    node: dict[str, Any] = {"name": rel, "type": "file"}
    if no_content:
        return node
    node["content"] = _read_file_content(file_path, max_file_bytes)
    return node


def _build_single_dir_tree(root_dir: Path, args: ParsedArgs) -> dict[str, Any]:
    from .ignore import get_ignore_specs, get_whitelist_spec
    from .tree import TreeBuildContext, build_tree

    ctx = TreeBuildContext(
        base_dir=root_dir,
        combined_spec=get_ignore_specs(
            root_dir, args.ignore_file, args.no_default_ignores, args.output_file, no_ignores=args.no_ignores
        ),
        output_file=args.output_file,
        max_depth=args.max_depth,
        no_content=args.no_content,
        max_file_bytes=args.max_file_bytes,
        whitelist_spec=get_whitelist_spec(args.whitelist_file, root_dir),
    )
    return {
        "name": _root_display_name(root_dir),
        "type": "directory",
        "children": build_tree(root_dir, ctx),
    }


def _build_standard_tree(args: ParsedArgs) -> dict[str, Any]:
    if not args.extra_dirs and not args.extra_files:
        return _build_single_dir_tree(args.root_dir, args)

    children: list[dict[str, Any]] = []

    for d in args.extra_dirs or []:
        children.append(_build_single_dir_tree(d, args))

    base = args.root_dir
    for f in args.extra_files or []:
        children.append(_build_file_node(f, base, args.no_content, args.max_file_bytes))

    if len(children) == 1:
        return children[0]

    return {"name": ".", "type": "directory", "children": children}


def _handle_clipboard(output_content: str, args: ParsedArgs, prog: str) -> bool:
    from .clipboard import ClipboardError, copy_to_clipboard

    if not args.copy:
        return False
    try:
        copy_to_clipboard(output_content)
        if not args.quiet:
            print("Copied to clipboard", file=sys.stderr)
        return True
    except ClipboardError as exc:
        print(f"{prog}: warning: clipboard unavailable ({exc}); writing to stdout instead", file=sys.stderr)
        return False


def _handle_output_file(output_content: str, args: ParsedArgs, prog: str) -> None:
    from .writer import write_string_to_file

    if not args.output_file:
        return
    try:
        write_string_to_file(output_content, args.output_file, args.output_format)
        if not args.quiet:
            print(f"Saved to {args.output_file}", file=sys.stderr)
    except IsADirectoryError:
        print(f"{prog}: error: '{args.output_file}' is a directory, not a file", file=sys.stderr)
        sys.exit(_EXIT_RUNTIME)
    except OSError as exc:
        print(f"{prog}: error: cannot write '{args.output_file}': {exc.strerror or exc}", file=sys.stderr)
        sys.exit(_EXIT_RUNTIME)


def _is_graph_mode(args: ParsedArgs) -> bool:
    return args.command == "graph"


_ARCHITECTURAL_EDGE_TYPES: frozenset[str] = frozenset(
    {"semantic", "structural", "config_generic", "document", "sibling", "test_edge", "history"}
)


def _format_cycles(level: str, pg: Any) -> str:
    from ._native.graph_analytics import detect_cycles

    cycles = detect_cycles(pg, level=level, edge_types={"semantic"})
    if not cycles:
        return "No dependency cycles detected."
    lines = [f"{len(cycles)} dependency cycle(s) detected:\n"]
    for i, cycle in enumerate(cycles, 1):
        chain = " → ".join(cycle) + " → " + cycle[0]
        lines.append(f"  Cycle {i} ({len(cycle)} nodes): {chain}")
    return "\n".join(lines)


def _format_hotspots(pg: Any) -> str:
    from ._native.graph_analytics import hotspots

    hot = hotspots(pg, top=10, edge_types=set(_ARCHITECTURAL_EDGE_TYPES))
    lines = [f"Top {len(hot)} hotspots:"]
    for rank, (name, score, details) in enumerate(hot, 1):
        lines.append(f"  {rank}. {name}  score={score:.4f}  out_degree={details['out_degree']}  churn={details['churn']}")
    return "\n".join(lines)


def _format_metrics(level: str, pg: Any) -> str:
    from ._native.graph_analytics import coupling_metrics

    metrics = coupling_metrics(pg, level=level, edge_types=set(_ARCHITECTURAL_EDGE_TYPES))
    lines = [f"Module metrics ({level} level):"]
    for m in metrics:
        flags = ""
        if m.coupling > 0.7:
            flags = "  ⚠ high coupling"
        elif m.cohesion > 0.8:
            flags = "  ✓ high cohesion"
        lines.append(
            f"  {m.name}  cohesion={m.cohesion:.3f}  coupling={m.coupling:.3f}  "
            f"instability={m.instability:.3f}  fan_in={m.fan_in}  fan_out={m.fan_out}{flags}"
        )
    return "\n".join(lines)


def _graph_to_string(pg: Any, fmt: str, level: str = "directory") -> str:
    from ._native.graph_analytics import quotient_graph, to_mermaid
    from ._native.graph_export import graph_to_graphml_string, graph_to_json_string

    if fmt == "graphml":
        return graph_to_graphml_string(pg)
    if fmt == "mermaid":
        qg = quotient_graph(pg, level=level)
        return to_mermaid(qg)
    return graph_to_json_string(pg)


def _handle_graph_mode(args: ParsedArgs) -> str:
    from ._native.graph_export import graph_summary
    from ._native.project_graph import build_project_graph

    assert args.graph is not None
    g = args.graph

    pg = build_project_graph(
        args.root_dir,
        ignore_file=args.ignore_file,
        no_default_ignores=args.no_default_ignores,
        whitelist_file=args.whitelist_file,
    )

    parts: list[str] = []

    if g.summary:
        parts.append(graph_summary(pg))
        parts.append(_format_cycles(g.level, pg))
        parts.append(_format_hotspots(pg))
        parts.append(_format_metrics(g.level, pg))

    if not g.summary:
        parts.append(_graph_to_string(pg, g.format, level=g.level))

    return "\n".join(parts) + "\n" if parts else ""


def _run(argv: list[str] | None = None, *, prog: str = "diffctx", version: str = __version__) -> None:
    from .cli import parse_args
    from .logger import setup_logging
    from .tokens import print_token_summary
    from .writer import tree_to_string

    args = parse_args(argv, prog=prog, version=version)
    setup_logging(args.verbosity)

    if _is_graph_mode(args):
        output_content = _handle_graph_mode(args)
        if not args.quiet:
            print_token_summary(output_content)
        _emit(output_content, args, prog, write_stdout=sys.stdout.write)
        return

    if args.diff_range and args.mode == "locate":
        _run_locate_mode(args, prog)
        return

    directory_tree = _build_diff_tree(args, prog) if args.diff_range else _build_standard_tree(args)
    is_empty_diff_result = bool(args.diff_range) and _diff_result_is_empty(directory_tree)

    output_content = tree_to_string(directory_tree, args.output_format)
    if not args.quiet:
        print_token_summary(output_content)
        if args.diff_range:
            _report_raw_diff_share(directory_tree, prog, args)
        _warn_if_output_oversized(output_content, args)

    def _write_via_writer(content: str) -> None:
        from .writer import write_string_to_file

        write_string_to_file(content, None, args.output_format)

    _emit(output_content, args, prog, write_stdout=_write_via_writer)

    if is_empty_diff_result:
        sys.exit(_EXIT_EMPTY_DIFF)


def _run_locate_mode(args: ParsedArgs, prog: str) -> None:
    import json

    from ._native import build_locate
    from .tokens import print_token_summary

    _ensure_git_repo(args.root_dir, prog)
    payload = _call_with_wall_clock_deadline(
        lambda: build_locate(
            root_dir=args.root_dir,
            diff_range=args.diff_range or "HEAD",
            budget_tokens=args.budget,
            alpha=args.alpha,
            tau=args.tau,
            scoring_mode=args.scoring,
            timeout=args.timeout,
        ),
        args.timeout,
        prog,
    )
    output_content = payload if payload.endswith("\n") else payload + "\n"
    doc = json.loads(payload)
    is_empty = (
        doc.get("item_count", 0) == 0
        and not doc.get("deleted_files")
        and not doc.get("renamed_files")
        and not doc.get("lockfile_changes")
    )
    if is_empty:
        print(
            f"{prog}: diff produced no semantic context (clean working tree, binary-only, or files over the size cap)",
            file=sys.stderr,
        )
    if not args.quiet:
        print_token_summary(output_content)
    _emit(output_content, args, prog, write_stdout=sys.stdout.write)
    if is_empty:
        sys.exit(_EXIT_EMPTY_DIFF)


def _emit(
    output_content: str,
    args: ParsedArgs,
    prog: str,
    *,
    write_stdout: Callable[[str], object],
) -> None:
    clipboard_ok = _handle_clipboard(output_content, args, prog)
    _handle_output_file(output_content, args, prog)
    should_write_stdout = args.force_stdout or not args.copy or not clipboard_ok
    if not args.output_file and should_write_stdout:
        write_stdout(output_content)


_KNOWN_RUNTIME_ERRORS: tuple[type[BaseException], ...] = (
    FileNotFoundError,
    IsADirectoryError,
    NotADirectoryError,
    PermissionError,
    RuntimeError,
)


def _format_runtime_error(exc: BaseException) -> str:
    msg = str(exc).strip()
    return msg if msg else exc.__class__.__name__


def _format_git_error(exc: BaseException) -> str:
    msg = _format_runtime_error(exc)
    if "unknown revision" in msg:
        match = re.search(r"ambiguous argument '([^']+)'", msg)
        if match:
            return f"unknown git revision '{match.group(1)}'; check refs with: git log --oneline"
    return f"git error: {msg}" if not msg.startswith("git ") else msg


def _handle_unexpected_exception(exc: BaseException, prog: str = "diffctx") -> int:
    logger.debug("internal error", exc_info=exc)
    print(
        f"{prog}: internal error: {exc.__class__.__name__}: {_format_runtime_error(exc)}",
        file=sys.stderr,
    )
    return _EXIT_RUNTIME


def _git_error_type() -> type[BaseException]:
    from typing import cast

    from ._native import GitError

    return cast("type[BaseException]", GitError)


def run(argv: list[str] | None = None, *, prog: str | None = None, version: str | None = None) -> None:
    prog = prog or "diffctx"
    version = version or __version__
    _configure_windows_utf8()
    try:
        _run(argv, prog=prog, version=version)
    except SystemExit:
        raise
    except KeyboardInterrupt:
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        print("\nInterrupted", file=sys.stderr)
        sys.exit(_EXIT_INTERRUPTED)
    except BrokenPipeError:
        sys.exit(_EXIT_BROKEN_PIPE)
    except _git_error_type() as exc:
        print(f"{prog}: {_format_git_error(exc)}", file=sys.stderr)
        sys.exit(_EXIT_ENVIRONMENT)
    except argparse.ArgumentError as exc:
        print(f"{prog}: usage error: {_format_runtime_error(exc)}", file=sys.stderr)
        sys.exit(_EXIT_USAGE)
    except _KNOWN_RUNTIME_ERRORS as exc:
        print(f"{prog}: {_format_runtime_error(exc)}", file=sys.stderr)
        sys.exit(_EXIT_RUNTIME)
    except OSError as exc:
        print(f"{prog}: {_format_runtime_error(exc)}", file=sys.stderr)
        sys.exit(_EXIT_RUNTIME)
    except Exception as exc:
        sys.exit(_handle_unexpected_exception(exc, prog=prog))


def main() -> None:
    run()


if __name__ == "__main__":
    main()
