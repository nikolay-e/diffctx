from __future__ import annotations

import io
import json
from typing import IO

from .project_graph import ProjectGraph


def write_graph_json(pg: ProjectGraph, output: IO[str]) -> None:
    data = pg.to_dict()
    json.dump(data, output, indent=2)
    output.write("\n")


def graph_to_json_string(pg: ProjectGraph) -> str:
    buf = io.StringIO()
    write_graph_json(pg, buf)
    return buf.getvalue()


def _escape_graphml(text: str) -> str:
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace('"', "&quot;")


def write_graph_graphml(pg: ProjectGraph, output: IO[str]) -> None:
    data = pg.to_dict()
    output.write('<?xml version="1.0" encoding="UTF-8"?>\n')
    output.write('<graphml xmlns="http://graphml.graphdrawing.org/graphml"\n')
    output.write('         xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"\n')
    output.write(
        '         xsi:schemaLocation="http://graphml.graphdrawing.org/graphml '
        'http://graphml.graphdrawing.org/dtds/graphml.dtd">\n'
    )

    output.write('  <key id="d_path" for="node" attr.name="path" attr.type="string"/>\n')
    output.write('  <key id="d_lines" for="node" attr.name="lines" attr.type="string"/>\n')
    output.write('  <key id="d_kind" for="node" attr.name="kind" attr.type="string"/>\n')
    output.write('  <key id="d_symbol" for="node" attr.name="symbol" attr.type="string"/>\n')
    output.write('  <key id="d_tokens" for="node" attr.name="token_count" attr.type="int"/>\n')
    output.write('  <key id="d_weight" for="edge" attr.name="weight" attr.type="double"/>\n')
    output.write('  <key id="d_category" for="edge" attr.name="category" attr.type="string"/>\n')
    output.write(f'  <graph id="{_escape_graphml(data["name"])}" edgedefault="directed">\n')

    for node in data["nodes"]:
        nid = _escape_graphml(node["id"])
        output.write(f'    <node id="{nid}">\n')
        output.write(f'      <data key="d_path">{_escape_graphml(node["path"])}</data>\n')
        output.write(f'      <data key="d_lines">{_escape_graphml(node["lines"])}</data>\n')
        output.write(f'      <data key="d_kind">{_escape_graphml(node["kind"])}</data>\n')
        if node.get("symbol"):
            output.write(f'      <data key="d_symbol">{_escape_graphml(node["symbol"])}</data>\n')
        output.write(f'      <data key="d_tokens">{node["token_count"]}</data>\n')
        output.write("    </node>\n")

    for i, edge in enumerate(data["edges"]):
        src = _escape_graphml(edge["source"])
        tgt = _escape_graphml(edge["target"])
        output.write(f'    <edge id="e{i}" source="{src}" target="{tgt}">\n')
        output.write(f'      <data key="d_weight">{edge["weight"]}</data>\n')
        output.write(f'      <data key="d_category">{_escape_graphml(edge["category"])}</data>\n')
        output.write("    </edge>\n")

    output.write("  </graph>\n")
    output.write("</graphml>\n")


def graph_to_graphml_string(pg: ProjectGraph) -> str:
    buf = io.StringIO()
    write_graph_graphml(pg, buf)
    return buf.getvalue()


def graph_summary(pg: ProjectGraph) -> str:
    lines: list[str] = []
    lines.append(f"{pg.node_count} nodes, {pg.edge_count} edges across {len(pg.files)} files")

    type_counts = pg.edge_type_counts()
    if type_counts:
        parts = [f"{cat} {count}" for cat, count in sorted(type_counts.items(), key=lambda x: -x[1])]
        lines.append(f"Edge types: {', '.join(parts)}")

    top = pg.top_by_in_degree(5)
    if top:
        top_parts = []
        for fid, deg in top:
            frag = pg.fragments.get(fid)
            label = frag.symbol_name if frag and frag.symbol_name else f"{fid.path.name}:{fid.start_line}"
            top_parts.append(f"{label} ({deg})")
        lines.append(f"Top by in-degree: {', '.join(top_parts)}")

    return "\n".join(lines)
