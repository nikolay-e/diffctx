from __future__ import annotations

import logging
from collections import deque
from typing import Any

import numpy as np

from .graph import CSRGraph, Graph
from .types import FragmentId

logger = logging.getLogger(__name__)


def _init_seed_residuals_csr(
    seeds: set[FragmentId],
    node_to_idx: dict[FragmentId, int],
    n: int,
    seed_weights: dict[FragmentId, float] | None,
) -> Any:
    residual = np.zeros(n, dtype=np.float64)
    valid_seeds = [s for s in seeds if s in node_to_idx]
    if not valid_seeds:
        return residual

    if seed_weights:
        epsilon = 0.1
        total_sw = sum(seed_weights.get(s, epsilon) for s in valid_seeds)
        if total_sw <= 0:
            return residual
        for s in valid_seeds:
            residual[node_to_idx[s]] = seed_weights.get(s, epsilon) / total_sw
    else:
        weight = 1.0 / len(valid_seeds)
        for s in valid_seeds:
            residual[node_to_idx[s]] = weight

    return residual


def _ppr_push_csr(
    csr: CSRGraph,
    seeds: set[FragmentId],
    alpha: float = 0.60,
    tol: float = 1e-4,
    seed_weights: dict[FragmentId, float] | None = None,
) -> Any:
    n = csr.n
    if n == 0:
        return np.empty(0, dtype=np.float64)

    restart = 1.0 - alpha
    push_threshold = tol

    indptr = csr.indptr
    indices = csr.indices
    weights = csr.weights
    out_sum = csr.out_weight_sum

    residual = _init_seed_residuals_csr(seeds, csr.node_to_idx, n, seed_weights)
    estimate = np.zeros(n, dtype=np.float64)
    in_queue = np.zeros(n, dtype=np.bool_)

    queue: deque[int] = deque()
    for i in range(n):
        if residual[i] >= push_threshold:
            queue.append(i)
            in_queue[i] = True

    pushes = 0
    max_pushes = min(n * 100, 2_000_000)

    while queue and pushes < max_pushes:
        u = queue.popleft()
        in_queue[u] = False

        r_u = residual[u]
        if r_u < push_threshold:
            continue

        estimate[u] += restart * r_u
        residual[u] = 0.0

        total_w = out_sum[u]
        if total_w <= 0:
            pushes += 1
            continue

        push_mass = alpha * r_u
        start = indptr[u]
        end = indptr[u + 1]

        for k in range(start, end):
            v = indices[k]
            delta = push_mass * (weights[k] / total_w)
            old_r = residual[v]
            new_r = old_r + delta
            residual[v] = new_r
            if not in_queue[v] and new_r >= push_threshold:
                queue.append(v)
                in_queue[v] = True

        pushes += 1

    return estimate


def personalized_pagerank(
    graph: Graph,
    seeds: set[FragmentId],
    alpha: float = 0.60,
    tol: float = 1e-4,
    lam: float = 0.4,
    seed_weights: dict[FragmentId, float] | None = None,
) -> dict[FragmentId, float]:
    if not graph.nodes:
        return {}

    valid_seeds = seeds & graph.nodes
    if not valid_seeds:
        logger.warning("PPR: none of %d seeds found in graph (%d nodes) — returning uniform", len(seeds), len(graph.nodes))
        return {n: 1.0 / len(graph.nodes) for n in graph.nodes}

    fwd_csr, rev_csr = graph.to_csr()

    forward_est = _ppr_push_csr(fwd_csr, valid_seeds, alpha, tol, seed_weights)
    backward_est = _ppr_push_csr(rev_csr, valid_seeds, alpha, tol, seed_weights)

    combined = lam * forward_est + (1.0 - lam) * backward_est

    total = combined.sum()
    if total > 0:
        combined /= total

    idx_to_node = fwd_csr.idx_to_node
    result: dict[FragmentId, float] = {}
    for i in range(fwd_csr.n):
        score = combined[i]
        if score > 0:
            result[idx_to_node[i]] = float(score)

    logger.debug(
        "PPR CSR bidirectional: n=%d forward_nonzero=%d backward_nonzero=%d result=%d nodes",
        fwd_csr.n,
        int(np.count_nonzero(forward_est)),
        int(np.count_nonzero(backward_est)),
        len(result),
    )

    return result
