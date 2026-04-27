from __future__ import annotations

from typing import Any, cast

from _diffctx import (
    graph_summary as _rust_graph_summary,
)
from _diffctx import (
    graph_to_graphml_string as _rust_graph_to_graphml_string,
)
from _diffctx import (
    graph_to_json_string as _rust_graph_to_json_string,
)


def graph_to_json_string(pg: Any) -> str:
    return cast(str, _rust_graph_to_json_string(pg))


def graph_to_graphml_string(pg: Any) -> str:
    return cast(str, _rust_graph_to_graphml_string(pg))


def graph_summary(pg: Any, top_n: int = 10) -> str:
    """Return a human-readable summary of the project graph.

    Wraps the Rust GraphSummary dict into the formatted text the previous
    Python implementation produced (used by the `treemapper graph` CLI).
    """
    s = _rust_graph_summary(pg, top_n)
    lines = [
        "Project graph summary:",
        f"  Nodes: {s['node_count']}  Edges: {s['edge_count']}  Files: {s['file_count']}",
        f"  Density: {s['density']:.4f}",
    ]
    edge_counts = s.get("edge_type_counts") or {}
    if edge_counts:
        lines.append("  Edge categories:")
        for cat, n in sorted(edge_counts.items(), key=lambda x: (-x[1], x[0])):
            lines.append(f"    {cat}: {n}")
    top = s.get("top_in_degree") or []
    if top:
        lines.append(f"  Top {len(top)} most-referenced:")
        for entry in top:
            lines.append(f"    {entry['label']}  in_degree={entry['in_degree']}")
    return "\n".join(lines)
