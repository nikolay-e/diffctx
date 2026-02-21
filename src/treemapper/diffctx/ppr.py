from __future__ import annotations

import logging
from collections import deque

from .graph import Graph
from .types import FragmentId


def _transpose_graph(graph: Graph) -> Graph:
    transposed = Graph()
    transposed.nodes = set(graph.nodes)
    for src, neighbors in graph.adjacency.items():
        for dst, weight in neighbors.items():
            transposed.add_edge(dst, src, weight)
    return transposed


def _init_seed_residuals(
    seeds: set[FragmentId],
    graph_nodes: set[FragmentId],
    seed_weights: dict[FragmentId, float] | None,
) -> dict[FragmentId, float]:
    valid_seeds = seeds & graph_nodes
    if not valid_seeds:
        return {}

    if seed_weights:
        total_sw = sum(seed_weights.get(s, 1.0) for s in valid_seeds)
        if total_sw <= 0:
            return {s: 0.0 for s in valid_seeds}
        return {s: seed_weights.get(s, 1.0) / total_sw for s in valid_seeds}

    weight = 1.0 / len(valid_seeds)
    return {s: weight for s in valid_seeds}


def _propagate_residual(
    u: FragmentId,
    residual: dict[FragmentId, float],
    estimate: dict[FragmentId, float],
    graph: Graph,
    alpha: float,
    restart: float,
    push_threshold: float,
    queue: deque[FragmentId],
    visited: set[FragmentId],
) -> None:
    r_u = residual.get(u, 0.0)
    if r_u < push_threshold:
        return

    estimate[u] = estimate.get(u, 0.0) + restart * r_u
    residual[u] = 0.0

    nbrs = graph.neighbors(u)
    total_weight = sum(nbrs.values()) if nbrs else 0.0
    if total_weight <= 0:
        return

    push_mass = alpha * r_u
    for v, w in nbrs.items():
        delta = push_mass * (w / total_weight)
        old_r = residual.get(v, 0.0)
        residual[v] = old_r + delta
        if v not in visited and old_r + delta >= push_threshold:
            queue.append(v)
            visited.add(v)


def _personalized_pagerank_sparse(
    graph: Graph,
    seeds: set[FragmentId],
    alpha: float = 0.60,
    tol: float = 1e-4,
    seed_weights: dict[FragmentId, float] | None = None,
) -> dict[FragmentId, float]:
    n = len(graph.nodes)
    if n == 0:
        return {}

    restart = 1.0 - alpha
    push_threshold = tol / n

    residual = _init_seed_residuals(seeds, graph.nodes, seed_weights)
    estimate: dict[FragmentId, float] = {}

    queue: deque[FragmentId] = deque(residual.keys())
    visited: set[FragmentId] = set(queue)

    pushes = 0
    max_pushes = n * 50
    while queue and pushes < max_pushes:
        u = queue.popleft()
        visited.discard(u)
        _propagate_residual(u, residual, estimate, graph, alpha, restart, push_threshold, queue, visited)
        pushes += 1

    return estimate


def personalized_pagerank(
    graph: Graph,
    seeds: set[FragmentId],
    alpha: float = 0.60,
    tol: float = 1e-4,
    lam: float = 0.5,
    seed_weights: dict[FragmentId, float] | None = None,
) -> dict[FragmentId, float]:
    if not graph.nodes:
        return {}

    valid_seeds = seeds & graph.nodes
    if not valid_seeds:
        return {n: 1.0 / len(graph.nodes) for n in graph.nodes}

    forward_scores = _personalized_pagerank_sparse(graph, valid_seeds, alpha, tol, seed_weights)

    transposed = _transpose_graph(graph)
    backward_scores = _personalized_pagerank_sparse(transposed, valid_seeds, alpha, tol, seed_weights)

    combined: dict[FragmentId, float] = {}
    all_nodes = set(forward_scores) | set(backward_scores)
    for node in all_nodes:
        fwd = forward_scores.get(node, 0.0)
        bwd = backward_scores.get(node, 0.0)
        combined[node] = lam * fwd + (1 - lam) * bwd

    total = sum(combined.values())
    if total > 0:
        return {n: s / total for n, s in combined.items()}

    logging.debug(
        "PPR sparse bidirectional: forward=%d backward=%d combined=%d nodes",
        len(forward_scores),
        len(backward_scores),
        len(combined),
    )
    return combined
