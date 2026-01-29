from __future__ import annotations

import logging
import math

from .graph import Graph
from .types import FragmentId


def _initialize_ppr_scores(
    nodes: list[FragmentId], valid_seeds: set[FragmentId]
) -> tuple[dict[FragmentId, float], dict[FragmentId, float]]:
    p = {n: (1.0 / len(valid_seeds) if n in valid_seeds else 0.0) for n in nodes}
    return p, dict(p)


def _ppr_iteration(
    nodes: list[FragmentId],
    graph: Graph,
    scores: dict[FragmentId, float],
    out_sum: dict[FragmentId, float],
    base: dict[FragmentId, float],
    p: dict[FragmentId, float],
    alpha: float,
) -> dict[FragmentId, float]:
    new_scores: dict[FragmentId, float] = dict(base)
    dangling_mass = 0.0

    for src in nodes:
        nbrs = graph.neighbors(src)
        total = out_sum[src]
        if total <= 0 or not nbrs:
            dangling_mass += scores[src]
            continue
        contrib = alpha * scores[src]
        for dst, w in nbrs.items():
            new_scores[dst] += contrib * (w / total)

    if dangling_mass > 0:
        add = alpha * dangling_mass
        for n in nodes:
            new_scores[n] += add * p[n]

    return new_scores


def _normalize_scores(scores: dict[FragmentId, float]) -> dict[FragmentId, float]:
    total = sum(scores.values())
    if total > 0:
        return {n: s / total for n, s in scores.items()}
    return scores


def personalized_pagerank(
    graph: Graph,
    seeds: set[FragmentId],
    alpha: float = 0.60,
    tol: float = 1e-4,
    max_iter: int = 50,
) -> dict[FragmentId, float]:
    if not graph.nodes:
        return {}

    nodes = list(graph.nodes)
    valid_seeds = seeds & graph.nodes
    if not valid_seeds:
        return {n: 1.0 / len(nodes) for n in nodes}

    p, scores = _initialize_ppr_scores(nodes, valid_seeds)
    out_sum = {}
    for n in nodes:
        nbr_values = list(graph.neighbors(n).values())
        finite_weights = [w for w in nbr_values if math.isfinite(w)]
        if len(finite_weights) < len(nbr_values):
            logging.debug("Node %s has non-finite edge weights, filtering", n)
        total = sum(finite_weights)
        out_sum[n] = total if math.isfinite(total) else 0.0
    base = {n: (1.0 - alpha) * p[n] for n in nodes}

    iteration = 0
    delta = float("inf")

    for iteration in range(max_iter):
        new_scores = _ppr_iteration(nodes, graph, scores, out_sum, base, p, alpha)
        delta = sum(abs(new_scores[n] - scores[n]) for n in nodes)
        scores = new_scores
        if delta < tol:
            logging.debug(
                "PPR converged at iteration %d (delta=%.6f, tol=%.6f, nodes=%d)",
                iteration + 1,
                delta,
                tol,
                len(nodes),
            )
            return _normalize_scores(scores)

    logging.warning(
        "PPR reached max_iter=%d without convergence (delta=%.6f, tol=%.6f, nodes=%d)",
        max_iter,
        delta,
        tol,
        len(nodes),
    )
    return _normalize_scores(scores)
