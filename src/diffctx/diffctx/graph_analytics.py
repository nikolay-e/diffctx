from __future__ import annotations

import re
import subprocess
from typing import Any, cast

from diffctx._diffctx import (
    coupling_metrics as _rust_coupling_metrics,
)
from diffctx._diffctx import (
    hotspots as _rust_hotspots,
)
from diffctx._diffctx import (
    quotient_graph as _rust_quotient_graph,
)
from diffctx._diffctx import (
    to_mermaid as _rust_to_mermaid,
)

from .project_graph import graph_root

QuotientGraph = Any  # opaque PyQuotientGraph from the Rust extension

_DENSE_EDGE_TYPES = frozenset({"config", "config_generic"})
_BACKLINK_DOMINANCE_RATIO = 0.8
_HOTSPOT_DEGREE_WEIGHT = 0.5
_HOTSPOT_CHURN_WEIGHT = 0.5
_CHURN_WINDOW = "12 months"

_MERMAID_NODE_LINE = re.compile(r'^\s*(n\d+)\["(.*)"\]\s*$')
_MERMAID_EDGE_LINE = re.compile(r'^\s*(n\d+) -->\|"(.+): ([0-9.]+)"\| (n\d+)\s*$')

_QUOTIENT_SOURCES: dict[int, tuple[Any, str]] = {}
_QUOTIENT_SOURCES_MAX = 16


def detect_cycles(
    pg: Any,
    level: str = "directory",
    edge_types: set[str] | None = None,
) -> list[list[str]]:
    qg = _rust_quotient_graph(pg, level)
    labels, edges = _parse_mermaid(_full_mermaid(qg))
    pair_weights = {(src, dst): weight for src, dst, _, weight in edges}

    adjacency: dict[str, list[str]] = {nid: [] for nid in labels}
    for src, dst, category, weight in edges:
        if edge_types is not None and category not in edge_types:
            continue
        opposing = pair_weights.get((dst, src), 0.0)
        if weight >= opposing * _BACKLINK_DOMINANCE_RATIO:
            adjacency[src].append(dst)

    names = _quotient_node_keys(labels, edges, _rust_coupling_metrics(pg, level, None))
    return [[names[nid] for nid in component] for component in _strongly_connected_components(adjacency) if len(component) > 1]


def hotspots(
    pg: Any,
    top: int = 10,
    edge_types: set[str] | None = None,
) -> list[tuple[str, float, dict[str, int]]]:
    types = _informative_edge_types(edge_types)
    entries = cast(
        list[tuple[str, float, dict[str, int]]],
        _rust_hotspots(pg, max(top, int(pg.fragment_count)), sorted(types) if types else None),
    )
    churn_by_file = _recent_commit_counts(graph_root(pg))

    max_degree = max((details["out_degree"] for _, _, details in entries), default=0) or 1
    max_churn = max(churn_by_file.values(), default=0) or 1

    rescored: list[tuple[str, float, dict[str, int]]] = []
    for path, _, details in entries:
        churn = churn_by_file.get(path, 0)
        details["churn"] = churn
        score = _HOTSPOT_DEGREE_WEIGHT * details["out_degree"] / max_degree + _HOTSPOT_CHURN_WEIGHT * churn / max_churn
        rescored.append((path, round(score, 4), details))

    rescored.sort(key=lambda entry: (-entry[1], entry[0]))
    return rescored[:top]


def coupling_metrics(
    pg: Any,
    level: str = "directory",
    edge_types: set[str] | None = None,
) -> list[Any]:
    types = sorted(edge_types) if edge_types else None
    return cast(list[Any], _rust_coupling_metrics(pg, level, types))


def quotient_graph(pg: Any, level: str = "directory") -> QuotientGraph:
    qg = _rust_quotient_graph(pg, level)
    if len(_QUOTIENT_SOURCES) >= _QUOTIENT_SOURCES_MAX:
        _QUOTIENT_SOURCES.clear()
    _QUOTIENT_SOURCES[id(qg)] = (pg, level)
    return qg


def to_mermaid(qg: Any, top_n: int = 50) -> str:
    text = cast(str, _rust_to_mermaid(qg, top_n))
    source = _QUOTIENT_SOURCES.get(id(qg))
    if source is not None:
        pg, level = source
        labels, edges = _parse_mermaid(_full_mermaid(qg))
        keys = _quotient_node_keys(labels, edges, _rust_coupling_metrics(pg, level, None))
        text = _relabel_mermaid_nodes(text, keys)
    return _normalize_mermaid_edge_weights(text)


def _informative_edge_types(edge_types: set[str] | None) -> set[str] | None:
    if not edge_types:
        return None
    kept = set(edge_types) - _DENSE_EDGE_TYPES
    return kept or set(edge_types)


def _recent_commit_counts(root: Any) -> dict[str, int]:
    if root is None:
        return {}
    try:
        proc = subprocess.run(
            [
                "git",
                "-C",
                str(root),
                "-c",
                "core.quotepath=off",
                "log",
                f"--since={_CHURN_WINDOW}",
                "--relative",
                "--name-only",
                "--pretty=format:",
            ],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            check=False,
            timeout=60,
        )
    except (OSError, subprocess.TimeoutExpired):
        return {}
    if proc.returncode != 0:
        return {}
    counts: dict[str, int] = {}
    for line in proc.stdout.splitlines():
        path = line.strip()
        if path:
            counts[path] = counts.get(path, 0) + 1
    return counts


def _full_mermaid(qg: Any) -> str:
    return cast(str, _rust_to_mermaid(qg, max(int(qg.node_count), 1)))


def _parse_mermaid(text: str) -> tuple[dict[str, str], list[tuple[str, str, str, float]]]:
    labels: dict[str, str] = {}
    edges: list[tuple[str, str, str, float]] = []
    for line in text.splitlines():
        node_match = _MERMAID_NODE_LINE.match(line)
        if node_match:
            labels[node_match[1]] = node_match[2]
            continue
        edge_match = _MERMAID_EDGE_LINE.match(line)
        if edge_match:
            edges.append((edge_match[1], edge_match[4], edge_match[2], float(edge_match[3])))
    return labels, edges


def _quotient_node_keys(
    labels: dict[str, str],
    edges: list[tuple[str, str, str, float]],
    metrics: list[Any],
) -> dict[str, str]:
    in_degree: dict[str, int] = {}
    out_degree: dict[str, int] = {}
    for src, dst, _, _ in edges:
        out_degree[src] = out_degree.get(src, 0) + 1
        in_degree[dst] = in_degree.get(dst, 0) + 1

    by_basename: dict[str, list[Any]] = {}
    for m in metrics:
        basename = m.name.rstrip("/").rsplit("/", 1)[-1] or m.name
        by_basename.setdefault(basename, []).append(m)

    nids_by_label: dict[str, list[str]] = {}
    for nid, label in labels.items():
        nids_by_label.setdefault(label, []).append(nid)

    keys: dict[str, str] = {}
    for label, nids in nids_by_label.items():
        candidates = by_basename.get(label, [])
        if len(candidates) < len(nids):
            for nid in nids:
                keys[nid] = label
            continue
        ranked_nids = sorted(nids, key=lambda n: (-(in_degree.get(n, 0) + out_degree.get(n, 0)), n))
        ranked_candidates = sorted(candidates, key=lambda m: (-(m.fan_in + m.fan_out), m.name))
        for nid, m in zip(ranked_nids, ranked_candidates):
            keys[nid] = m.name
    return keys


def _relabel_mermaid_nodes(text: str, keys: dict[str, str]) -> str:
    lines: list[str] = []
    for line in text.splitlines():
        m = _MERMAID_NODE_LINE.match(line)
        if m and m[1] in keys:
            line = f'    {m[1]}["{keys[m[1]]}"]'
        lines.append(line)
    return "\n".join(lines) + "\n"


def _strongly_connected_components(adjacency: dict[str, list[str]]) -> list[list[str]]:
    next_index = 0
    indices: dict[str, int] = {}
    lowlinks: dict[str, int] = {}
    on_stack: set[str] = set()
    stack: list[str] = []
    components: list[list[str]] = []

    for root in adjacency:
        if root in indices:
            continue
        indices[root] = lowlinks[root] = next_index
        next_index += 1
        stack.append(root)
        on_stack.add(root)
        work: list[tuple[str, Any]] = [(root, iter(adjacency[root]))]
        while work:
            node, neighbors = work[-1]
            descended = False
            for neighbor in neighbors:
                if neighbor not in indices:
                    indices[neighbor] = lowlinks[neighbor] = next_index
                    next_index += 1
                    stack.append(neighbor)
                    on_stack.add(neighbor)
                    work.append((neighbor, iter(adjacency[neighbor])))
                    descended = True
                    break
                if neighbor in on_stack:
                    lowlinks[node] = min(lowlinks[node], indices[neighbor])
            if descended:
                continue
            work.pop()
            if lowlinks[node] == indices[node]:
                component: list[str] = []
                while True:
                    member = stack.pop()
                    on_stack.discard(member)
                    component.append(member)
                    if member == node:
                        break
                components.append(component)
            if work:
                parent = work[-1][0]
                lowlinks[parent] = min(lowlinks[parent], lowlinks[node])
    return components


def _normalize_mermaid_edge_weights(text: str) -> str:
    lines = text.splitlines()
    weights = [float(m[3]) for m in map(_MERMAID_EDGE_LINE.match, lines) if m]
    max_weight = max(weights, default=0.0)
    if max_weight <= 0:
        return text
    normalized: list[str] = []
    for line in lines:
        m = _MERMAID_EDGE_LINE.match(line)
        if m:
            share = float(m[3]) / max_weight
            label = "<1%" if share < 0.01 else f"{share:.0%}"
            line = f'    {m[1]} -->|"{m[2]}: {label}"| {m[4]}'
        normalized.append(line)
    return "\n".join(normalized) + "\n"
