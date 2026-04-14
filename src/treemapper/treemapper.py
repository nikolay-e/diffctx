from __future__ import annotations

import logging
import sys
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from .cli import GraphArgs, ParsedArgs

logger = logging.getLogger(__name__)


def _configure_windows_utf8() -> None:
    if sys.platform == "win32" and hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")


def _build_diff_tree(args: ParsedArgs) -> dict[str, Any]:
    from .diffctx import GitError, build_diff_context

    if not args.diff_range:
        raise RuntimeError("diff_range is required in diff mode")
    try:
        return build_diff_context(
            root_dir=args.root_dir,
            diff_range=args.diff_range,
            budget_tokens=args.budget,
            alpha=args.alpha,
            tau=args.tau,
            no_content=args.no_content,
            ignore_file=args.ignore_file,
            no_default_ignores=args.no_default_ignores,
            full=args.full_diff,
            whitelist_file=args.whitelist_file,
            scoring_mode=getattr(args, "scoring", "hybrid"),
        )
    except GitError as e:
        logger.error("%s", e)
        sys.exit(1)


def _root_display_name(root_dir: Any) -> str:
    name = root_dir.name
    return name if name else str(root_dir)


def _build_standard_tree(args: ParsedArgs) -> dict[str, Any]:
    from .ignore import get_ignore_specs, get_whitelist_spec
    from .tree import TreeBuildContext, build_tree

    ctx = TreeBuildContext(
        base_dir=args.root_dir,
        combined_spec=get_ignore_specs(args.root_dir, args.ignore_file, args.no_default_ignores, args.output_file),
        output_file=args.output_file,
        max_depth=args.max_depth,
        no_content=args.no_content,
        max_file_bytes=args.max_file_bytes,
        whitelist_spec=get_whitelist_spec(args.whitelist_file, args.root_dir),
    )
    return {
        "name": _root_display_name(args.root_dir),
        "type": "directory",
        "children": build_tree(args.root_dir, ctx),
    }


def _handle_clipboard(output_content: str, args: ParsedArgs) -> bool:
    from .clipboard import ClipboardError, copy_to_clipboard

    if not args.copy:
        return False
    try:
        copy_to_clipboard(output_content)
        if not args.quiet:
            print("Copied to clipboard", file=sys.stderr)
        return True
    except ClipboardError as e:
        logger.error("Clipboard unavailable: %s", e)
        return False


def _handle_output_file(output_content: str, args: ParsedArgs) -> None:
    from .writer import write_string_to_file

    if not args.output_file:
        return
    try:
        write_string_to_file(output_content, args.output_file, args.output_format)
        if not args.quiet:
            print(f"Saved to {args.output_file}", file=sys.stderr)
    except IsADirectoryError:
        logger.error("'%s' is a directory", args.output_file)
        sys.exit(1)
    except OSError as e:
        logger.error("Cannot write '%s': %s", args.output_file, e)
        sys.exit(1)


def _is_graph_mode(args: ParsedArgs) -> bool:
    return args.command == "graph"


_ARCHITECTURAL_EDGE_TYPES: frozenset[str] = frozenset(
    {"semantic", "structural", "config_generic", "document", "sibling", "test_edge", "history"}
)


def _format_cycles(g: GraphArgs, pg: Any) -> str:
    from .diffctx.graph_analytics import detect_cycles

    cycles = detect_cycles(pg, level=g.level, edge_types={"semantic"})
    if not cycles:
        return "No dependency cycles detected."
    lines = [f"{len(cycles)} dependency cycle(s) detected:\n"]
    for i, cycle in enumerate(cycles, 1):
        chain = " \u2192 ".join(cycle) + " \u2192 " + cycle[0]
        lines.append(f"  Cycle {i} ({len(cycle)} nodes): {chain}")
    return "\n".join(lines)


def _format_hotspots(_g: GraphArgs, pg: Any) -> str:
    from .diffctx.graph_analytics import hotspots

    hot = hotspots(pg, top=10, edge_types=set(_ARCHITECTURAL_EDGE_TYPES))
    lines = [f"Top {len(hot)} hotspots:"]
    for rank, (name, score, details) in enumerate(hot, 1):
        lines.append(f"  {rank}. {name}  score={score}  out_degree={details['out_degree']}  churn={details['churn']}")
    return "\n".join(lines)


def _format_metrics(g: GraphArgs, pg: Any) -> str:
    from .diffctx.graph_analytics import coupling_metrics

    metrics = coupling_metrics(pg, level=g.level, edge_types=set(_ARCHITECTURAL_EDGE_TYPES))
    lines = [f"Module metrics ({g.level} level):"]
    for m in metrics:
        flags = ""
        if m.coupling > 0.7:
            flags = "  \u26a0 high coupling"
        elif m.cohesion > 0.8:
            flags = "  \u2713 high cohesion"
        lines.append(
            f"  {m.name}  cohesion={m.cohesion}  coupling={m.coupling}  "
            f"instability={m.instability}  fan_in={m.fan_in}  fan_out={m.fan_out}{flags}"
        )
    return "\n".join(lines)


def _graph_to_string(pg: Any, fmt: str, level: str = "directory") -> str:
    from .diffctx.graph_analytics import quotient_graph, to_mermaid
    from .diffctx.graph_export import graph_to_graphml_string, graph_to_json_string

    if fmt == "graphml":
        return graph_to_graphml_string(pg)
    if fmt == "mermaid":
        qg = quotient_graph(pg, level=level)
        return to_mermaid(qg)
    return graph_to_json_string(pg)


def _handle_graph_mode(args: ParsedArgs) -> str:
    from .diffctx.graph_export import graph_summary
    from .diffctx.project_graph import build_project_graph

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
        parts.append(_format_cycles(g, pg))
        parts.append(_format_hotspots(g, pg))
        parts.append(_format_metrics(g, pg))

    if not g.summary:
        parts.append(_graph_to_string(pg, g.format, level=g.level))

    return "\n".join(parts) + "\n" if parts else ""


def _run() -> None:
    from .cli import parse_args
    from .logger import setup_logging
    from .tokens import print_token_summary
    from .writer import tree_to_string

    args = parse_args()
    setup_logging(args.verbosity)

    if _is_graph_mode(args):
        output_content = _handle_graph_mode(args)
        clipboard_ok = _handle_clipboard(output_content, args)
        _handle_output_file(output_content, args)
        should_write_stdout = args.force_stdout or not args.copy or not clipboard_ok
        if not args.output_file and should_write_stdout:
            sys.stdout.write(output_content)
        return

    directory_tree = _build_diff_tree(args) if args.diff_range else _build_standard_tree(args)

    output_content = tree_to_string(directory_tree, args.output_format)
    if not args.quiet:
        print_token_summary(output_content)

    clipboard_ok = _handle_clipboard(output_content, args)
    _handle_output_file(output_content, args)

    should_write_stdout = args.force_stdout or not args.copy or not clipboard_ok
    if not args.output_file and should_write_stdout:
        from .writer import write_string_to_file

        write_string_to_file(output_content, None, args.output_format)


def main() -> None:
    _configure_windows_utf8()
    try:
        _run()
    except KeyboardInterrupt:
        print("\nInterrupted", file=sys.stderr)
        sys.exit(130)
    except BrokenPipeError:
        sys.exit(141)


if __name__ == "__main__":
    main()
