from __future__ import annotations

import logging
import math
from dataclasses import dataclass, field
from pathlib import Path

from .config import LEXICAL
from .edges import collect_all_edges
from .edges.similarity.lexical import clamp_lexical_weight
from .embeddings import _build_embedding_edges
from .types import Fragment, FragmentId


@dataclass
class Graph:
    adjacency: dict[FragmentId, dict[FragmentId, float]] = field(default_factory=dict)
    nodes: set[FragmentId] = field(default_factory=set)

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

    plugin_edges = collect_all_edges(fragments, repo_root)
    for (src, dst), weight in plugin_edges.items():
        all_edges[(src, dst)] = max(all_edges.get((src, dst), 0.0), weight)

    embedding_edges = _build_embedding_edges(fragments, clamp_lexical_weight)
    for (src, dst), weight in embedding_edges.items():
        all_edges[(src, dst)] = max(all_edges.get((src, dst), 0.0), weight)

    all_edges = _apply_hub_suppression(all_edges)

    for (src, dst), weight in all_edges.items():
        graph.add_edge(src, dst, weight)

    return graph


def _apply_hub_suppression(
    edges: dict[tuple[FragmentId, FragmentId], float],
) -> dict[tuple[FragmentId, FragmentId], float]:
    if not edges:
        return edges

    in_degree: dict[FragmentId, int] = {}
    for src, dst in edges.keys():
        in_degree[dst] = in_degree.get(dst, 0) + 1

    if not in_degree:
        return edges

    sorted_degrees = sorted(in_degree.values())
    threshold_idx = int(len(sorted_degrees) * LEXICAL.hub_percentile)
    threshold = sorted_degrees[min(threshold_idx, len(sorted_degrees) - 1)]

    suppressed: dict[tuple[FragmentId, FragmentId], float] = {}
    for (src, dst), weight in edges.items():
        if in_degree.get(dst, 0) > threshold:
            weight = weight / math.log(1 + in_degree[dst])
        suppressed[(src, dst)] = weight

    return suppressed
