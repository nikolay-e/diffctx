from __future__ import annotations

from typing import Any, cast

from diffctx._diffctx import (
    graph_summary as _rust_graph_summary,
)
from diffctx._diffctx import (
    graph_to_graphml_string as _rust_graph_to_graphml_string,
)
from diffctx._diffctx import (
    graph_to_json_string as _rust_graph_to_json_string,
)


def graph_to_json_string(pg: Any) -> str:
    return cast(str, _rust_graph_to_json_string(pg))


def graph_to_graphml_string(pg: Any) -> str:
    return cast(str, _rust_graph_to_graphml_string(pg))


def graph_summary(pg: Any, top_n: int = 10) -> str:
    s = _rust_graph_summary(pg, top_n)
    lines = [
        "Project graph summary:",
        f"  Nodes: {s['node_count']}  Edges: {s['edge_count']}  Files: {s['file_count']}",
        f"  Density: {s['density']:.4f}",
    ]
    edge_counts = s.get("edge_type_counts") or {}
    if edge_counts:
        total = sum(edge_counts.values())
        lines.append("  Edge categories (% of discovered relations):")
        for cat, n in sorted(edge_counts.items(), key=lambda x: (-x[1], x[0])):
            lines.append(f"    {cat}: {_category_share(n, total)}")
    top = _informative_top_referenced(s.get("top_in_degree") or [], int(s["node_count"]))
    if top:
        lines.append(f"  Top {len(top)} most-referenced:")
        for entry in top:
            lines.append(f"    {entry['label']}  in_degree={entry['in_degree']}")
    return "\n".join(lines)


def _category_share(count: int, total: int) -> str:
    share = 100.0 * count / total if total else 0.0
    return f"{share:.1f}%" if share >= 0.05 else "<0.1%"


def _informative_top_referenced(top: list[dict[str, Any]], node_count: int) -> list[dict[str, Any]]:
    entries = [e for e in top if e["in_degree"] < max(node_count - 1, 1)]
    if len(entries) > 1 and len({e["in_degree"] for e in entries}) == 1:
        return []
    return entries
