from __future__ import annotations

import logging
import math
from pathlib import Path

import networkx as nx

from .config import LIMITS
from .edges import collect_all_edges
from .edges.similarity.lexical import clamp_lexical_weight
from .embeddings import _build_embedding_edges
from .types import Fragment, FragmentId

logger = logging.getLogger(__name__)


class Graph:
    def __init__(self) -> None:
        self._g = nx.DiGraph()
        self.edge_categories: dict[tuple[FragmentId, FragmentId], str] = {}
        self._adj_cache: dict[FragmentId, dict[FragmentId, float]] | None = None
        self._rev_cache: dict[FragmentId, dict[FragmentId, float]] | None = None

    def _invalidate_cache(self) -> None:
        self._adj_cache = None
        self._rev_cache = None

    @property
    def nodes(self) -> set[FragmentId]:
        return set(self._g.nodes)

    @property
    def adjacency(self) -> dict[FragmentId, dict[FragmentId, float]]:
        if self._adj_cache is not None:
            return self._adj_cache
        result: dict[FragmentId, dict[FragmentId, float]] = {}
        for src in self._g:
            nbrs = {}
            for dst, data in self._g[src].items():
                nbrs[dst] = data.get("weight", 0.0)
            if nbrs:
                result[src] = nbrs
        self._adj_cache = result
        return result

    @property
    def reverse_adjacency(self) -> dict[FragmentId, dict[FragmentId, float]]:
        if self._rev_cache is not None:
            return self._rev_cache
        result: dict[FragmentId, dict[FragmentId, float]] = {}
        for dst in self._g:
            preds = {}
            for src in self._g.predecessors(dst):
                preds[src] = self._g[src][dst].get("weight", 0.0)
            if preds:
                result[dst] = preds
        self._rev_cache = result
        return result

    def add_node(self, node: FragmentId) -> None:
        self._g.add_node(node)

    def add_edge(self, src: FragmentId, dst: FragmentId, weight: float) -> None:
        if math.isnan(weight) or math.isinf(weight) or weight <= 0:
            logger.debug("Dropping edge %s -> %s: invalid weight %r", src, dst, weight)
            return
        existing = self._g[src][dst]["weight"] if self._g.has_edge(src, dst) else 0.0
        self._g.add_edge(src, dst, weight=max(existing, weight))
        self._invalidate_cache()

    def neighbors(self, node: FragmentId) -> dict[FragmentId, float]:
        if node not in self._g:
            return {}
        return {dst: self._g[node][dst].get("weight", 0.0) for dst in self._g.successors(node)}

    @property
    def nx(self) -> nx.DiGraph:
        return self._g

    def ego_graph(self, seeds: set[FragmentId], radius: int = 2) -> dict[FragmentId, float]:
        scores: dict[FragmentId, float] = {}
        for seed in seeds:
            if seed not in self._g:
                continue
            undirected = self._g.to_undirected(as_view=True)
            ego = nx.ego_graph(undirected, seed, radius=radius)
            for node in ego.nodes:
                dist = nx.shortest_path_length(undirected, seed, node)
                hop_score = 1.0 / (1 + dist) if dist > 0 else 1.0
                scores[node] = max(scores.get(node, 0.0), hop_score)
        return scores

    def pagerank(
        self, seeds: set[FragmentId], alpha: float = 0.6, seed_weights: dict[FragmentId, float] | None = None
    ) -> dict[FragmentId, float]:
        if not self._g.nodes:
            return {}
        valid_seeds = seeds & set(self._g.nodes)
        if not valid_seeds:
            return {n: 1.0 / len(self._g) for n in self._g}
        personalization: dict[FragmentId, float] = {}
        if seed_weights:
            total = sum(seed_weights.get(s, 1.0) for s in valid_seeds)
            for s in valid_seeds:
                personalization[s] = seed_weights.get(s, 1.0) / total if total > 0 else 1.0 / len(valid_seeds)
        else:
            for s in valid_seeds:
                personalization[s] = 1.0 / len(valid_seeds)
        try:
            scores: dict[FragmentId, float] = nx.pagerank(
                self._g, alpha=1 - alpha, personalization=personalization, max_iter=200, tol=1e-6
            )
            return scores
        except nx.PowerIterationFailedConvergence:
            logger.warning("PageRank failed to converge, falling back to uniform")
            return {n: 1.0 / len(self._g) for n in self._g}


def build_graph(fragments: list[Fragment], repo_root: Path | None = None) -> Graph:
    graph = Graph()

    for frag in fragments:
        graph.add_node(frag.id)

    skip_expensive = len(fragments) > LIMITS.skip_expensive_threshold
    if skip_expensive:
        logger.debug("diffctx: %d fragments exceed threshold, skipping expensive edge builders", len(fragments))

    all_edges: dict[tuple[FragmentId, FragmentId], float] = {}
    edge_categories: dict[tuple[FragmentId, FragmentId], str] = {}

    plugin_edges, plugin_categories = collect_all_edges(fragments, repo_root, skip_expensive=skip_expensive)
    for (src, dst), weight in plugin_edges.items():
        if weight > all_edges.get((src, dst), 0.0):
            all_edges[(src, dst)] = weight
            edge_categories[(src, dst)] = plugin_categories.get((src, dst), "generic")

    if not skip_expensive:
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


_SUPPRESSION_EXEMPT = frozenset({"semantic", "structural", "test_edge"})


_HUB_OUT_DEGREE_THRESHOLD = 3


def _suppress_high_in_degree(
    edges: dict[tuple[FragmentId, FragmentId], float],
    edge_categories: dict[tuple[FragmentId, FragmentId], str],
) -> dict[tuple[FragmentId, FragmentId], float]:
    in_degree: dict[FragmentId, int] = {}
    for _src, dst in edges:
        in_degree[dst] = in_degree.get(dst, 0) + 1

    degrees = sorted(in_degree.values())
    mid = len(degrees) // 2
    d_median = (degrees[mid] + degrees[~mid]) / 2.0

    suppressed: dict[tuple[FragmentId, FragmentId], float] = {}
    for (src, dst), weight in edges.items():
        deg = in_degree.get(dst, 0)
        if deg > d_median and edge_categories.get((src, dst), "generic") not in _SUPPRESSION_EXEMPT:
            weight = weight / max(1.0, math.log(1 + deg))
        suppressed[(src, dst)] = weight
    return suppressed


def _suppress_semantic_hubs(
    edges: dict[tuple[FragmentId, FragmentId], float],
    edge_categories: dict[tuple[FragmentId, FragmentId], str],
) -> dict[tuple[FragmentId, FragmentId], float]:
    sem_out_files: dict[FragmentId, set[Path]] = {}
    for (src, dst), cat in edge_categories.items():
        if cat == "semantic":
            sem_out_files.setdefault(src, set()).add(dst.path)

    for src, dst in list(edges):
        out_file_deg = len(sem_out_files.get(src, set()))
        if out_file_deg >= _HUB_OUT_DEGREE_THRESHOLD and edge_categories.get((src, dst)) == "semantic":
            edges[(src, dst)] = edges[(src, dst)] / math.sqrt(out_file_deg)
    return edges


def _apply_hub_suppression(
    edges: dict[tuple[FragmentId, FragmentId], float],
    edge_categories: dict[tuple[FragmentId, FragmentId], str],
) -> dict[tuple[FragmentId, FragmentId], float]:
    if not edges:
        return edges
    suppressed = _suppress_high_in_degree(edges, edge_categories)
    return _suppress_semantic_hubs(suppressed, edge_categories)
