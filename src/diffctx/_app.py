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

from ._diffctx import BUILD_LABEL as _BUILD_LABEL

if TYPE_CHECKING:
    from ._native.pipeline import Delivery
    from .cli import ParsedArgs

_T = TypeVar("_T")

logger = logging.getLogger(__name__)

_EXIT_RUNTIME = 1
_EXIT_USAGE = 2
_EXIT_ENVIRONMENT = 3
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
            ["git", "rev-parse", "--is-bare-repository"],
            stdin=subprocess.DEVNULL,
            cwd=str(root_dir),
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            check=False,
        )
    except OSError as exc:
        print(
            f"{prog}: --diff requires git to be installed and on PATH ({exc}); install git or run without --diff.",
            file=sys.stderr,
        )
        sys.exit(_EXIT_ENVIRONMENT)
    if result.returncode != 0:
        fatal = next((line for line in result.stderr.splitlines() if line.startswith("fatal:")), "")
        if fatal and "not a git repository" not in fatal:
            print(f"{prog}: --diff cannot run git in {root_dir}: {fatal}", file=sys.stderr)
            sys.exit(_EXIT_ENVIRONMENT)
        print(
            f"{prog}: --diff requires a git repository (cwd: {root_dir}); "
            "run inside a working tree or pass --diff <range> with a valid git context.",
            file=sys.stderr,
        )
        sys.exit(_EXIT_ENVIRONMENT)
    if result.stdout.strip() == "true":
        print(
            f"{prog}: --diff requires a working tree (bare repository: {root_dir}); "
            "clone it with a working tree or run without --diff.",
            file=sys.stderr,
        )
        sys.exit(_EXIT_ENVIRONMENT)

    head_result = subprocess.run(
        ["git", "rev-parse", "--verify", "-q", "HEAD"],
        stdin=subprocess.DEVNULL,
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


def _log_latency(latency: dict[str, Any] | None) -> None:
    # Telemetry rides beside the artifact, never inside it: the native binary
    # never renders it and `--budget` bounds the document alone.
    if latency:
        logger.info("latency (ms): %s", ", ".join(f"{key}={value}" for key, value in latency.items()))


def _say(notes: list[tuple[str, str]], quiet: bool, prog: str) -> None:
    # What to say, and what is empty, partial or redacted, is the engine's
    # (`diffctx::delivery`); the door only names itself and honours `-q`.
    for kind, text in notes:
        if kind == "warning":
            print(f"{prog}: {text}", file=sys.stderr)
        elif not quiet:
            print(f"{prog}: {text}" if kind == "detail" else text, file=sys.stderr)


_WATCHDOG_GRACE_SECONDS = 30


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

    # The engine stops cooperatively at the deadline and returns a partial
    # answer; the watchdog waits a grace period beyond it, as the binary
    # does, or it would kill exactly the answer the deadline exists to save
    # (#382). The test variable replaces the whole wait.
    grace = _WATCHDOG_GRACE_SECONDS
    wait = timeout_seconds + grace
    override = os.environ.get("DIFFCTX_TEST_WATCHDOG_GRACE_SECS")
    # ASCII digits within u64, as the binary reads it: "²" and "+5" are no
    # override, and a wait past threading.TIMEOUT_MAX is clamped, not raised.
    if override and override.isascii() and override.isdigit() and int(override) < 2**64:
        grace = wait = min(int(override), int(threading.TIMEOUT_MAX))
    thread = threading.Thread(target=worker, name="diffctx-pipeline", daemon=True)
    thread.start()
    thread.join(wait)
    if thread.is_alive():
        print(
            f"{prog}: pipeline exceeded {timeout_seconds}s wall-clock deadline and did not stop "
            f"cooperatively within {grace}s more; aborting before OOM/SIGKILL. Narrow the review "
            "with an explicit '--diff <from>..<to>' range, run on a smaller subtree, or raise '--timeout'.",
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


def _diff_scope(args: ParsedArgs) -> list[str]:
    return [str(p.resolve()) for p in [*(args.extra_dirs or []), *(args.extra_files or [])]]


def _root_display_name(root_dir: Any) -> str:
    from .tree import printable

    name = root_dir.name
    return printable(name if name else str(root_dir))


_LARGE_OUTPUT_WARN_BYTES = 10 * 1024 * 1024
# A bare `diffctx` with no path argument at all is the most common "just try
# it" first invocation — and the easiest to run somewhere unintended (e.g.
# /tmp, $HOME). Use a much lower bar for that specific case; an explicit path
# argument is a stronger signal the user knows what they're pointing at (#87).
_LARGE_OUTPUT_WARN_BYTES_NO_EXPLICIT_PATH = 1 * 1024 * 1024


def _warn_if_output_oversized(output_content: str, args: ParsedArgs) -> None:
    if args.no_content:
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
    from .tree import printable, set_file_content

    try:
        rel = file_path.relative_to(base_dir).as_posix()
    except ValueError:
        try:
            rel = file_path.relative_to(Path.cwd()).as_posix()
        except ValueError:
            rel = file_path.name
    node: dict[str, Any] = {"name": printable(rel), "type": "file"}
    if no_content:
        return node
    set_file_content(node, file_path, max_file_bytes)
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

    # Several directories have no single real parent, so "." names the
    # synthetic root honestly. Files alone DO have one — the common parent the
    # CLI rooted them at — and the tree says so.
    name = "." if args.extra_dirs else _root_display_name(args.root_dir)
    return {"name": name, "type": "directory", "children": children}


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
    {"semantic", "structural", "key_match", "config_generic", "document", "sibling", "test_edge", "history"}
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


def _graph_to_string(pg: Any, fmt: str, level: str = "directory", export_level: str = "fragment") -> str:
    from ._native.graph_analytics import quotient_graph, to_mermaid
    from ._native.graph_export import graph_to_graphml_string, graph_to_json_string

    if fmt == "graphml":
        return graph_to_graphml_string(pg, level=export_level)
    if fmt == "mermaid":
        qg = quotient_graph(pg, level=level)
        return to_mermaid(qg)
    return graph_to_json_string(pg, level=export_level)


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
        parts.extend(
            [
                graph_summary(pg, level=g.export_level),
                _format_cycles(g.level, pg),
                _format_hotspots(pg),
                _format_metrics(g.level, pg),
            ]
        )

    if not g.summary:
        parts.append(_graph_to_string(pg, g.format, level=g.level, export_level=g.export_level))

    return "\n".join(parts) + "\n" if parts else ""


def _run(argv: list[str] | None = None, *, prog: str = "diffctx", version: str = _BUILD_LABEL) -> None:
    from .cli import parse_args
    from .logger import setup_logging
    from .tokens import print_token_summary
    from .writer import tree_to_string, write_string_to_file

    args = parse_args(argv, prog=prog, version=version)
    setup_logging(args.verbosity)

    def write_via_writer(content: str) -> None:
        write_string_to_file(content, None, args.output_format)

    if _is_graph_mode(args):
        output_content = _handle_graph_mode(args)
        if not args.quiet:
            print_token_summary(output_content)
        _emit(output_content, args, prog, write_stdout=sys.stdout.write)
        return

    if args.diff_range or (args.symbol and args.mode == "impact"):
        delivery = _deliver_diff_mode(args, prog)
        _log_latency(delivery.latency)
        _say(delivery.notes, args.quiet, prog)
        _emit(delivery.text, args, prog, write_stdout=write_via_writer)
        if delivery.exit:
            sys.exit(delivery.exit)
        return

    directory_tree = _build_standard_tree(args)
    output_content = tree_to_string(directory_tree, args.output_format)
    if not args.quiet:
        print_token_summary(output_content)
        _report_redactions(directory_tree, prog)
        _warn_if_output_oversized(output_content, args)
    _emit(output_content, args, prog, write_stdout=write_via_writer)


def _report_redactions(tree: dict[str, Any], prog: str) -> None:
    from .tree import redaction_total

    count = redaction_total(tree)
    if count:
        print(f"{prog}: {count} credential-shaped string(s) redacted", file=sys.stderr)


def _deliver_diff_mode(args: ParsedArgs, prog: str) -> Delivery:
    """Every diff mode is one engine call: this door parses, waits for the
    deadline and prints; what the answer says and how the run ends is decided
    in `diffctx::delivery`."""
    from ._native import deliver

    _ensure_git_repo(args.root_dir, prog)
    try:
        return _call_with_wall_clock_deadline(
            lambda: deliver(
                root_dir=args.root_dir,
                mode=args.mode,
                diff_range=args.diff_range or "HEAD",
                symbol=args.symbol,
                paths=_diff_scope(args),
                budget=args.budget,
                format=args.output_format,
                unfold=args.unfold,
                alpha=args.alpha,
                tau=args.tau,
                scoring_mode=args.scoring,
                timeout=args.timeout,
                no_content=args.no_content,
                full=args.full_diff,
                with_raw_diff=args.with_raw_diff,
                overflow=args.overflow,
            ),
            args.timeout,
            prog,
        )
    except ValueError as exc:
        if args.mode != "impact":
            raise
        # The engine refuses a `--symbol` that cannot name a definition.
        print(f"{prog}: usage error: {exc}", file=sys.stderr)
        sys.exit(_EXIT_USAGE)


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
    if "unknown revision" in msg and "shallow clone" not in msg:
        match = re.search(r"ambiguous argument '([^']+)'|unknown revision '([^']+)'", msg)
        if match:
            return f"unknown git revision '{match.group(1) or match.group(2)}'; check refs with: git log --oneline"
    return f"git error: {msg}" if not msg.startswith("git ") else msg


def _is_usage_git_error(exc: BaseException) -> bool:
    return _format_runtime_error(exc).startswith(("invalid duration", "invalid path"))


def _handle_unexpected_exception(exc: BaseException, prog: str = "diffctx") -> int:
    logger.debug("internal error", exc_info=exc)
    print(
        f"{prog}: internal error: {exc.__class__.__name__}: {_format_runtime_error(exc)}",
        file=sys.stderr,
    )
    return _EXIT_RUNTIME


# Never raised: the type an `except` clause gets when the extension is missing.
class _NativeUnavailableError(Exception):
    pass


def _git_error_type() -> type[BaseException]:
    # Evaluated while an exception is already propagating: if the native
    # module is what failed, importing it here would raise INSIDE the except
    # clause and bury the original error under an ImportError. A type nothing
    # raises matches nothing and lets the real error reach the handlers below.
    try:
        from ._native import GitError
    except ImportError:
        return _NativeUnavailableError
    return GitError


def run(argv: list[str] | None = None, *, prog: str | None = None, version: str | None = None) -> None:
    prog = prog or "diffctx"
    version = version or _BUILD_LABEL
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
        sys.exit(_EXIT_USAGE if _is_usage_git_error(exc) else _EXIT_ENVIRONMENT)
    except argparse.ArgumentError as exc:
        print(f"{prog}: usage error: {_format_runtime_error(exc)}", file=sys.stderr)
        sys.exit(_EXIT_USAGE)
    except _KNOWN_RUNTIME_ERRORS as exc:
        print(f"{prog}: {_format_runtime_error(exc)}", file=sys.stderr)
        sys.exit(_EXIT_RUNTIME)
    except LookupError as exc:
        # The engine refuses a name nothing defines with a bare LookupError;
        # a KeyError or IndexError is a bug here and keeps its traceback.
        if type(exc) is not LookupError:
            sys.exit(_handle_unexpected_exception(exc, prog=prog))
        print(f"{prog}: {_format_runtime_error(exc)}", file=sys.stderr)
        sys.exit(_EXIT_RUNTIME)
    except OSError as exc:
        print(f"{prog}: {_format_runtime_error(exc)}", file=sys.stderr)
        sys.exit(_EXIT_RUNTIME)
    except Exception as exc:
        sys.exit(_handle_unexpected_exception(exc, prog=prog))
