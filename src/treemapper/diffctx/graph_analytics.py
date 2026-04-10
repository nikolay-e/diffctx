from __future__ import annotations

import subprocess
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path

import networkx as nx

from .project_graph import ProjectGraph, _relative_path
from .types import Fragment, FragmentId


@dataclass
class QuotientNode:
    key: str
    label: str = ""
    fragment_count: int = 0
    token_count: int = 0
    self_weight: float = 0.0


@dataclass
class QuotientEdge:
    source: str
    target: str
    weight: float = 0.0
    categories: dict[str, int] = field(default_factory=dict)


@dataclass
class QuotientGraph:
    nodes: dict[str, QuotientNode] = field(default_factory=dict)
    edges: dict[tuple[str, str], QuotientEdge] = field(default_factory=dict)
    level: str = "directory"


def _node_label(fid: FragmentId, frag: Fragment, level: str) -> str:
    if level == "fragment":
        basename = fid.path.name
        if frag.symbol_name:
            return f"{frag.symbol_name} ({basename}:{fid.start_line})"
        return f"{basename}:{fid.start_line}-{fid.end_line}"
    if level == "file":
        return fid.path.name
    return Path(_group_key(fid, level, None)).name or "."


def _group_key(fid: FragmentId, level: str, root_dir: Path | None) -> str:
    if level == "fragment":
        rel = _relative_path(fid.path, root_dir)
        return f"{rel}:{fid.start_line}-{fid.end_line}"
    if level == "file":
        return _relative_path(fid.path, root_dir)
    return _relative_path(fid.path.parent, root_dir) or "."


def quotient_graph(pg: ProjectGraph, level: str = "directory") -> QuotientGraph:
    qg = QuotientGraph(level=level)
    root = pg.root_dir

    fid_to_group: dict[FragmentId, str] = {}
    for fid, frag in pg.fragments.items():
        key = _group_key(fid, level, root)
        fid_to_group[fid] = key
        if key not in qg.nodes:
            qg.nodes[key] = QuotientNode(key=key, label=_node_label(fid, frag, level))
        qg.nodes[key].fragment_count += 1
        qg.nodes[key].token_count += frag.token_count

    for src, nbrs in pg.graph.adjacency.items():
        src_key = fid_to_group.get(src)
        if src_key is None:
            continue
        for dst, weight in nbrs.items():
            dst_key = fid_to_group.get(dst)
            if dst_key is None:
                continue
            cat = pg.graph.edge_categories.get((src, dst), "generic")
            if src_key == dst_key:
                qg.nodes[src_key].self_weight += weight
            else:
                pair = (src_key, dst_key)
                if pair not in qg.edges:
                    qg.edges[pair] = QuotientEdge(source=src_key, target=dst_key)
                qg.edges[pair].weight += weight
                qg.edges[pair].categories[cat] = qg.edges[pair].categories.get(cat, 0) + 1

    return qg


def to_mermaid(qg: QuotientGraph, top_n: int = 20) -> str:
    if not qg.nodes:
        return "graph LR\n"

    sorted_nodes = sorted(
        qg.nodes.values(),
        key=lambda n: n.self_weight + sum(e.weight for e in qg.edges.values() if e.source == n.key or e.target == n.key),
        reverse=True,
    )[:top_n]
    node_keys = {n.key for n in sorted_nodes}

    node_ids: dict[str, str] = {}
    for i, node in enumerate(sorted_nodes):
        node_ids[node.key] = f"n{i}"

    lines = ["graph LR"]
    for node in sorted_nodes:
        nid = node_ids[node.key]
        label = node.label or node.key.rstrip("/") or "root"
        lines.append(f'    {nid}["{label}"]')

    sorted_edges = sorted(qg.edges.values(), key=lambda e: e.weight, reverse=True)
    for edge in sorted_edges:
        if edge.source not in node_keys or edge.target not in node_keys:
            continue
        src_id = node_ids[edge.source]
        dst_id = node_ids[edge.target]
        top_cat = max(edge.categories, key=lambda k: edge.categories[k]) if edge.categories else "?"
        weight = int(edge.weight) if edge.weight == int(edge.weight) else round(edge.weight, 1)
        lines.append(f'    {src_id} -->|"{top_cat}: {weight}"| {dst_id}')

    return "\n".join(lines) + "\n"


def _tarjan_scc(adjacency: dict[str, set[str]]) -> list[list[str]]:
    g = nx.DiGraph()
    for node, neighbors in adjacency.items():
        g.add_node(node)
        for nbr in neighbors:
            g.add_edge(node, nbr)
    return [list(c) for c in nx.strongly_connected_components(g) if len(c) > 1]


def detect_cycles(
    pg: ProjectGraph,
    level: str = "file",
    edge_types: set[str] | None = None,
) -> list[list[str]]:
    qg = quotient_graph(pg, level=level)

    adjacency: dict[str, set[str]] = defaultdict(set)
    for node_key in qg.nodes:
        adjacency.setdefault(node_key, set())
    for (src, dst), edge in qg.edges.items():
        if edge_types:
            matching = sum(c for cat, c in edge.categories.items() if cat in edge_types)
            if matching == 0:
                continue
        adjacency[src].add(dst)

    return _tarjan_scc(dict(adjacency))


def _compute_churn(root_dir: Path, max_commits: int = 200) -> dict[str, int]:
    try:
        result = subprocess.run(
            ["git", "-C", str(root_dir), "log", f"--max-count={max_commits}", "--numstat", "--no-renames", "--format="],
            capture_output=True,
            encoding="utf-8",
            errors="replace",
            timeout=30,
        )
        if result.returncode != 0:
            return {}
    except (subprocess.SubprocessError, OSError):
        return {}

    churn: dict[str, int] = defaultdict(int)
    for line in result.stdout.splitlines():
        parts = line.split("\t")
        if len(parts) != 3:
            continue
        added, deleted, file_path = parts
        try:
            churn[file_path] += int(added) + int(deleted)
        except ValueError:
            continue
    return dict(churn)


def hotspots(
    pg: ProjectGraph,
    top: int = 10,
    edge_types: set[str] | None = None,
) -> list[tuple[str, float, dict[str, float]]]:
    root = pg.root_dir

    out_deg: dict[str, int] = defaultdict(int)
    file_frag_count: dict[str, int] = defaultdict(int)
    for fid in pg.fragments:
        rel = _relative_path(fid.path, root)
        file_frag_count[rel] += 1
    for (src, _dst), cat in pg.graph.edge_categories.items():
        if edge_types is not None and cat not in edge_types:
            continue
        rel = _relative_path(src.path, root)
        out_deg[rel] += 1

    churn = _compute_churn(root) if root else {}

    all_files = set(file_frag_count.keys())
    max_deg = max(out_deg.values()) if out_deg else 1
    max_churn = max(churn.values()) if churn else 1

    scored: list[tuple[str, float, dict[str, float]]] = []
    for f in all_files:
        deg_norm = out_deg.get(f, 0) / max(max_deg, 1)
        churn_norm = churn.get(f, 0) / max(max_churn, 1)
        score = 0.5 * deg_norm + 0.5 * churn_norm
        scored.append((f, round(score, 4), {"out_degree": out_deg.get(f, 0), "churn": churn.get(f, 0)}))

    scored.sort(key=lambda x: x[1], reverse=True)
    return scored[:top]


@dataclass
class ModuleMetrics:
    name: str
    cohesion: float = 0.0
    coupling: float = 0.0
    instability: float = 0.0
    fan_in: int = 0
    fan_out: int = 0


def coupling_metrics(
    pg: ProjectGraph,
    level: str = "directory",
    edge_types: set[str] | None = None,
) -> list[ModuleMetrics]:
    qg = quotient_graph(pg, level=level)

    out_weight: dict[str, float] = defaultdict(float)
    in_weight: dict[str, float] = defaultdict(float)
    fan_in_set: dict[str, set[str]] = defaultdict(set)
    fan_out_set: dict[str, set[str]] = defaultdict(set)

    for (src, dst), edge in qg.edges.items():
        if edge_types is not None:
            matching = sum(c for cat, c in edge.categories.items() if cat in edge_types)
            if matching == 0:
                continue
        out_weight[src] += edge.weight
        in_weight[dst] += edge.weight
        fan_out_set[src].add(dst)
        fan_in_set[dst].add(src)

    results: list[ModuleMetrics] = []
    for key, node in sorted(qg.nodes.items()):
        intra = node.self_weight
        inter = out_weight.get(key, 0.0) + in_weight.get(key, 0.0)
        total = intra + inter
        cohesion = intra / total if total > 0 else 0.0
        coupling_val = inter / total if total > 0 else 0.0
        fi = len(fan_in_set.get(key, set()))
        fo = len(fan_out_set.get(key, set()))
        instability = fo / (fi + fo) if (fi + fo) > 0 else 0.0

        results.append(
            ModuleMetrics(
                name=key,
                cohesion=round(cohesion, 3),
                coupling=round(coupling_val, 3),
                instability=round(instability, 3),
                fan_in=fi,
                fan_out=fo,
            )
        )

    return results


def blast_radius(
    pg: ProjectGraph,
    seed_files: list[Path],
    max_depth: int = 3,
) -> dict[str, list[tuple[str, int]]]:
    root = pg.root_dir
    seed_paths = {f.resolve() for f in seed_files}
    seed_fids = {fid for fid in pg.fragments if fid.path.resolve() in seed_paths}

    result: dict[str, list[tuple[str, int]]] = {}
    visited: set[FragmentId] = set(seed_fids)
    prev_frontier: set[FragmentId] = seed_fids

    for depth in range(1, max_depth + 1):
        frontier: set[FragmentId] = set()
        for fid in prev_frontier:
            for dep in pg.graph.reverse_adjacency.get(fid, {}):
                if dep not in visited:
                    frontier.add(dep)
                    visited.add(dep)

        file_counts: dict[str, int] = defaultdict(int)
        for fid in frontier:
            file_counts[_relative_path(fid.path, root)] += 1

        result[f"depth_{depth}"] = sorted(file_counts.items())
        prev_frontier = frontier

    total_reachable_files = len(
        {_relative_path(fid.path, root) for fid in visited} - {_relative_path(fid.path, root) for fid in seed_fids}
    )
    total_files = len(pg.files)
    result["summary"] = [
        (f"reachable_files={total_reachable_files}/{total_files}", 0),
        (f"reachable_fragments={len(visited) - len(seed_fids)}/{pg.node_count}", 0),
    ]

    return result
