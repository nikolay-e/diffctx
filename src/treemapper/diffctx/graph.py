from __future__ import annotations

import logging
import math
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import networkx as nx
import numpy as np

from .config import LIMITS
from .types import Fragment, FragmentId

logger = logging.getLogger(__name__)

_NDArray = Any


def _build_csr(
    adj: dict[FragmentId, dict[FragmentId, float]],
    nodes: list[FragmentId],
    node_to_idx: dict[FragmentId, int],
) -> CSRGraph:
    n = len(nodes)
    total_edges = sum(len(v) for v in adj.values())
    indptr = np.zeros(n + 1, dtype=np.int32)
    indices = np.empty(total_edges, dtype=np.int32)
    weights = np.empty(total_edges, dtype=np.float64)
    k = 0
    for i in range(n):
        nbrs = adj.get(nodes[i])
        if nbrs:
            for dst, w in nbrs.items():
                dst_idx = node_to_idx.get(dst)
                if dst_idx is not None:
                    indices[k] = dst_idx
                    weights[k] = w
                    k += 1
        indptr[i + 1] = k
    if k < total_edges:
        indices = indices[:k]  # type: ignore[assignment,unused-ignore]
        weights = weights[:k]  # type: ignore[assignment,unused-ignore]
    out_sum = np.zeros(n, dtype=np.float64)
    for i in range(n):
        s, e = indptr[i], indptr[i + 1]
        if e > s:
            out_sum[i] = weights[s:e].sum()
    return CSRGraph(n, indptr, indices, weights, out_sum, node_to_idx, nodes)


@dataclass
class CSRGraph:
    n: int
    indptr: _NDArray
    indices: _NDArray
    weights: _NDArray
    out_weight_sum: _NDArray
    node_to_idx: dict[FragmentId, int] = field(repr=False)
    idx_to_node: list[FragmentId] = field(repr=False)


class Graph:
    def __init__(self) -> None:
        self._nodes: set[FragmentId] = set()
        self._fwd: dict[FragmentId, dict[FragmentId, float]] = {}
        self._rev: dict[FragmentId, dict[FragmentId, float]] = {}
        self.edge_categories: dict[tuple[FragmentId, FragmentId], str] = {}
        self._csr_cache: tuple[CSRGraph, CSRGraph] | None = None

    @property
    def nodes(self) -> set[FragmentId]:
        return self._nodes

    @property
    def adjacency(self) -> dict[FragmentId, dict[FragmentId, float]]:
        return self._fwd

    @property
    def reverse_adjacency(self) -> dict[FragmentId, dict[FragmentId, float]]:
        return self._rev

    def add_node(self, node: FragmentId) -> None:
        self._nodes.add(node)

    def add_edge(self, src: FragmentId, dst: FragmentId, weight: float) -> None:
        if math.isnan(weight) or math.isinf(weight) or weight <= 0:
            logger.debug("Dropping edge %s -> %s: invalid weight %r", src, dst, weight)
            return
        fwd_nbrs = self._fwd.get(src)
        if fwd_nbrs is None:
            fwd_nbrs = {}
            self._fwd[src] = fwd_nbrs
        existing = fwd_nbrs.get(dst, 0.0)
        new_weight = max(existing, weight)
        fwd_nbrs[dst] = new_weight

        rev_nbrs = self._rev.get(dst)
        if rev_nbrs is None:
            rev_nbrs = {}
            self._rev[dst] = rev_nbrs
        rev_nbrs[src] = new_weight

    def neighbors(self, node: FragmentId) -> dict[FragmentId, float]:
        return self._fwd.get(node, {})

    def to_csr(self) -> tuple[CSRGraph, CSRGraph]:
        if self._csr_cache is not None:
            return self._csr_cache
        nodes = sorted(self._nodes)
        node_to_idx = {n: i for i, n in enumerate(nodes)}
        result = (
            _build_csr(self._fwd, nodes, node_to_idx),
            _build_csr(self._rev, nodes, node_to_idx),
        )
        self._csr_cache = result
        return result

    @property
    def nx(self) -> nx.DiGraph[FragmentId]:
        g: nx.DiGraph[FragmentId] = nx.DiGraph()
        g.add_nodes_from(self._nodes)
        for src, nbrs in self._fwd.items():
            for dst, w in nbrs.items():
                g.add_edge(src, dst, weight=w)
        return g

    @staticmethod
    def _visit_adj(
        adj: dict[FragmentId, dict[FragmentId, float]],
        node: FragmentId,
        new_dist: int,
        visited: dict[FragmentId, int],
        next_frontier: dict[FragmentId, int],
    ) -> None:
        for nbr in adj.get(node, {}):
            if nbr not in visited:
                visited[nbr] = new_dist
                next_frontier[nbr] = new_dist

    def _bfs_from_seed(self, seed: FragmentId, radius: int) -> dict[FragmentId, int]:
        frontier = {seed: 0}
        visited: dict[FragmentId, int] = {seed: 0}
        for _step in range(radius):
            next_frontier: dict[FragmentId, int] = {}
            for node, dist in frontier.items():
                new_dist = dist + 1
                self._visit_adj(self._fwd, node, new_dist, visited, next_frontier)
                self._visit_adj(self._rev, node, new_dist, visited, next_frontier)
            frontier = next_frontier
        return visited

    def ego_graph(self, seeds: set[FragmentId], radius: int = 2) -> dict[FragmentId, float]:
        scores: dict[FragmentId, float] = {}
        if not self._nodes:
            return scores
        for seed in seeds:
            if seed not in self._nodes:
                continue
            visited = self._bfs_from_seed(seed, radius)
            for node, dist in visited.items():
                hop_score = 1.0 / (1 + dist) if dist > 0 else 1.0
                scores[node] = max(scores.get(node, 0.0), hop_score)
        return scores


def _merge_edges(
    target: dict[tuple[FragmentId, FragmentId], float],
    categories: dict[tuple[FragmentId, FragmentId], str],
    source: dict[tuple[FragmentId, FragmentId], float],
    source_cats: dict[tuple[FragmentId, FragmentId], str] | None = None,
    default_cat: str = "generic",
) -> None:
    for (src, dst), weight in source.items():
        if weight > target.get((src, dst), 0.0):
            target[(src, dst)] = weight
            if source_cats:
                categories[(src, dst)] = source_cats.get((src, dst), default_cat)
            else:
                categories[(src, dst)] = default_cat


def _populate_graph(graph: Graph, all_edges: dict[tuple[FragmentId, FragmentId], float]) -> None:
    for (src, dst), w in all_edges.items():
        if w > 0:
            fwd_nbrs = graph._fwd.get(src)
            if fwd_nbrs is None:
                fwd_nbrs = {}
                graph._fwd[src] = fwd_nbrs
            fwd_nbrs[dst] = w
            rev_nbrs = graph._rev.get(dst)
            if rev_nbrs is None:
                rev_nbrs = {}
                graph._rev[dst] = rev_nbrs
            rev_nbrs[src] = w


def build_graph(fragments: list[Fragment], repo_root: Path | None = None) -> Graph:
    from .edges import collect_all_edges
    from .edges.similarity.lexical import clamp_lexical_weight
    from .embeddings import _build_embedding_edges

    graph = Graph()
    for frag in fragments:
        graph.add_node(frag.id)

    skip_expensive = len(fragments) > LIMITS.skip_expensive_threshold
    if skip_expensive:
        logger.debug("diffctx: %d fragments exceed threshold, skipping expensive edge builders", len(fragments))

    all_edges: dict[tuple[FragmentId, FragmentId], float] = {}
    edge_categories: dict[tuple[FragmentId, FragmentId], str] = {}

    plugin_edges, plugin_categories = collect_all_edges(fragments, repo_root, skip_expensive=skip_expensive)
    _merge_edges(all_edges, edge_categories, plugin_edges, plugin_categories)

    if not skip_expensive:
        embedding_edges = _build_embedding_edges(fragments, clamp_lexical_weight)
        _merge_edges(all_edges, edge_categories, embedding_edges, default_cat="similarity")

    all_edges = _apply_hub_suppression(all_edges, edge_categories)
    _populate_graph(graph, all_edges)
    graph.edge_categories = edge_categories

    return graph


_SUPPRESSION_EXEMPT = frozenset({"semantic", "structural", "test_edge"})

_HUB_OUT_DEGREE_THRESHOLD = 3


def _build_node_index(
    edge_list: list[tuple[tuple[FragmentId, FragmentId], float]],
) -> dict[FragmentId, int]:
    node_set: dict[FragmentId, int] = {}
    for (src, dst), _w in edge_list:
        if src not in node_set:
            node_set[src] = len(node_set)
        if dst not in node_set:
            node_set[dst] = len(node_set)
    return node_set


def _build_edge_arrays(
    edge_list: list[tuple[tuple[FragmentId, FragmentId], float]],
    node_set: dict[FragmentId, int],
    edge_categories: dict[tuple[FragmentId, FragmentId], str],
) -> tuple[_NDArray, _NDArray, _NDArray, _NDArray, _NDArray]:
    n_edges = len(edge_list)
    src_idx = np.empty(n_edges, dtype=np.int32)
    dst_idx = np.empty(n_edges, dtype=np.int32)
    weights = np.empty(n_edges, dtype=np.float64)
    is_semantic = np.zeros(n_edges, dtype=np.bool_)
    is_exempt = np.zeros(n_edges, dtype=np.bool_)
    for i, ((src, dst), w) in enumerate(edge_list):
        src_idx[i] = node_set[src]
        dst_idx[i] = node_set[dst]
        weights[i] = w
        cat = edge_categories.get((src, dst), "generic")
        if cat == "semantic":
            is_semantic[i] = True
        if cat in _SUPPRESSION_EXEMPT:
            is_exempt[i] = True
    return src_idx, dst_idx, weights, is_semantic, is_exempt


def _suppress_semantic_hubs(
    edge_list: list[tuple[tuple[FragmentId, FragmentId], float]],
    src_idx: _NDArray,
    is_semantic: _NDArray,
    weights: _NDArray,
    n_nodes: int,
) -> None:
    sem_out_files: dict[int, set[Path]] = {}
    for i, ((_, dst), _w) in enumerate(edge_list):
        if is_semantic[i]:
            sem_out_files.setdefault(src_idx[i], set()).add(dst.path)
    if not sem_out_files:
        return
    sem_file_deg = np.zeros(n_nodes, dtype=np.int32)
    for si, files in sem_out_files.items():
        sem_file_deg[si] = len(files)
    src_sem_deg = sem_file_deg[src_idx]
    sem_hub_mask = is_semantic & (src_sem_deg >= _HUB_OUT_DEGREE_THRESHOLD)
    weights /= np.where(sem_hub_mask, np.sqrt(src_sem_deg.astype(np.float64)), 1.0)


def _apply_hub_suppression(
    edges: dict[tuple[FragmentId, FragmentId], float],
    edge_categories: dict[tuple[FragmentId, FragmentId], str],
) -> dict[tuple[FragmentId, FragmentId], float]:
    if not edges:
        return edges

    edge_list = list(edges.items())
    node_set = _build_node_index(edge_list)
    n_nodes = len(node_set)
    src_idx, dst_idx, weights, is_semantic, is_exempt = _build_edge_arrays(edge_list, node_set, edge_categories)

    in_degree = np.bincount(dst_idx, minlength=n_nodes)
    degrees_sorted = np.sort(in_degree[in_degree > 0])
    mid = len(degrees_sorted) // 2
    d_median = (degrees_sorted[mid] + degrees_sorted[~mid]) / 2.0
    dst_deg = in_degree[dst_idx]
    suppress_mask = (dst_deg > d_median) & ~is_exempt
    weights /= np.where(suppress_mask, np.maximum(1.0, np.log(1.0 + dst_deg)), 1.0)

    _suppress_semantic_hubs(edge_list, src_idx, is_semantic, weights, n_nodes)

    result: dict[tuple[FragmentId, FragmentId], float] = {}
    for i, ((src, dst), _w) in enumerate(edge_list):
        result[(src, dst)] = float(weights[i])
    return result
