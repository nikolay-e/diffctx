from __future__ import annotations

import logging
import math
from dataclasses import dataclass, field
from pathlib import Path

from .edges import collect_all_edges
from .edges.similarity.lexical import clamp_lexical_weight
from .embeddings import _build_embedding_edges
from .types import Fragment, FragmentId


@dataclass
class Graph:
    adjacency: dict[FragmentId, dict[FragmentId, float]] = field(default_factory=dict)
    nodes: set[FragmentId] = field(default_factory=set)
    edge_categories: dict[tuple[FragmentId, FragmentId], str] = field(default_factory=dict)

    def add_node(self, node: FragmentId) -> None:
        self.nodes.add(node)

    def add_edge(self, src: FragmentId, dst: FragmentId, weight: float) -> None:
        if math.isnan(weight) or math.isinf(weight) or weight <= 0:
            logging.debug("Dropping edge %s -> %s: invalid weight %r", src, dst, weight)
            return
        if src not in self.adjacency:
            self.adjacency[src] = {}
        existing = self.adjacency[src].get(dst, 0.0)
        self.adjacency[src][dst] = max(existing, weight)
        self.nodes.add(src)
        self.nodes.add(dst)

    def neighbors(self, node: FragmentId) -> dict[FragmentId, float]:
        return self.adjacency.get(node, {})


def build_graph(fragments: list[Fragment], repo_root: Path | None = None) -> Graph:
    graph = Graph()

    for frag in fragments:
        graph.nodes.add(frag.id)

    all_edges: dict[tuple[FragmentId, FragmentId], float] = {}
    edge_categories: dict[tuple[FragmentId, FragmentId], str] = {}

    plugin_edges, plugin_categories = collect_all_edges(fragments, repo_root)
    for (src, dst), weight in plugin_edges.items():
        if weight > all_edges.get((src, dst), 0.0):
            all_edges[(src, dst)] = weight
            edge_categories[(src, dst)] = plugin_categories.get((src, dst), "generic")

    embedding_edges = _build_embedding_edges(fragments, clamp_lexical_weight)
    for (src, dst), weight in embedding_edges.items():
        if weight > all_edges.get((src, dst), 0.0):
            all_edges[(src, dst)] = weight
            edge_categories[(src, dst)] = "similarity"

    all_edges = _apply_hub_suppression(all_edges, edge_categories)

    for (src, dst), weight in all_edges.items():
        graph.add_edge(src, dst, weight)

    graph.edge_categories = edge_categories

    return graph


_SUPPRESSION_EXEMPT = frozenset({"semantic", "structural", "config", "document"})


def _apply_hub_suppression(
    edges: dict[tuple[FragmentId, FragmentId], float],
    edge_categories: dict[tuple[FragmentId, FragmentId], str],
) -> dict[tuple[FragmentId, FragmentId], float]:
    if not edges:
        return edges

    in_degree: dict[FragmentId, int] = {}
    for _src, dst in edges.keys():
        in_degree[dst] = in_degree.get(dst, 0) + 1

    if not in_degree:
        return edges

    degrees = sorted(in_degree.values())
    mid = len(degrees) // 2
    d_median = (degrees[mid] + degrees[~mid]) / 2.0

    suppressed: dict[tuple[FragmentId, FragmentId], float] = {}
    for (src, dst), weight in edges.items():
        deg = in_degree.get(dst, 0)
        cat = edge_categories.get((src, dst), "generic")
        if deg > d_median and cat not in _SUPPRESSION_EXEMPT:
            weight = weight / max(1.0, math.log(1 + deg))
        suppressed[(src, dst)] = weight

    return suppressed
