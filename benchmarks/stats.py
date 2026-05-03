from __future__ import annotations

from collections.abc import Iterable

import numpy as np


def bootstrap_ci(values: list[float], n_iter: int = 10000, alpha: float = 0.05, seed: int = 42) -> tuple[float, float, float]:
    if not values:
        return (0.0, 0.0, 0.0)
    arr = np.asarray(values, dtype=np.float64)
    if len(arr) == 1:
        return (float(arr[0]), float(arr[0]), float(arr[0]))
    rng = np.random.default_rng(seed)
    # Vectorized: resample n_iter rows of size len(arr) at once, then row-mean.
    samples = rng.choice(arr, size=(n_iter, len(arr)), replace=True)
    means = samples.mean(axis=1)
    lo = float(np.percentile(means, 100 * alpha / 2))
    hi = float(np.percentile(means, 100 * (1 - alpha / 2)))
    return (float(arr.mean()), lo, hi)


def paired_bootstrap_delta(before: list[float], after: list[float], n_iter: int = 10000, seed: int = 42) -> dict:
    if not before or not after or len(before) != len(after):
        return {"delta_mean": 0.0, "ci_lo": 0.0, "ci_hi": 0.0, "p_value": 1.0}
    b = np.asarray(before, dtype=np.float64)
    a = np.asarray(after, dtype=np.float64)
    diffs = a - b
    if len(diffs) == 1:
        d = float(diffs[0])
        return {"delta_mean": d, "ci_lo": d, "ci_hi": d, "p_value": 0.0 if d > 0 else 1.0}
    rng = np.random.default_rng(seed)
    samples = rng.choice(diffs, size=(n_iter, len(diffs)), replace=True)
    boot_deltas = samples.mean(axis=1)
    return {
        "delta_mean": float(diffs.mean()),
        "ci_lo": float(np.percentile(boot_deltas, 2.5)),
        "ci_hi": float(np.percentile(boot_deltas, 97.5)),
        "p_value": float((boot_deltas <= 0).mean()),
    }


def wilcoxon_paired(before: list[float], after: list[float]) -> dict:
    if not before or not after or len(before) != len(after):
        return {"statistic": 0.0, "p_value": 1.0}
    b = np.asarray(before, dtype=np.float64)
    a = np.asarray(after, dtype=np.float64)
    diffs = a - b
    nonzero = np.count_nonzero(diffs)
    if nonzero < 2:
        return {"statistic": 0.0, "p_value": 1.0}
    from scipy.stats import wilcoxon as _wilcoxon

    try:
        stat, p = _wilcoxon(a, b)
        return {"statistic": float(stat), "p_value": float(p)}
    except ValueError:
        return {"statistic": 0.0, "p_value": 1.0}


def holm_correct(p_values: Iterable[float], alpha: float = 0.05) -> list[dict]:
    """Holm-Bonferroni step-down correction. Returns per-input dicts in input order:
        {"p_raw", "p_adj", "rejected"}.
    Use for prespecified primary tests where FWER control is required.
    """
    ps = list(p_values)
    n = len(ps)
    if n == 0:
        return []
    # Sort ascending; track original index.
    order = sorted(range(n), key=lambda i: ps[i])
    p_adj = [0.0] * n
    running_max = 0.0
    for rank, idx in enumerate(order):
        adj = (n - rank) * ps[idx]
        if adj > 1.0:
            adj = 1.0
        running_max = max(running_max, adj)
        p_adj[idx] = running_max
    return [{"p_raw": ps[i], "p_adj": p_adj[i], "rejected": p_adj[i] < alpha} for i in range(n)]


def bh_fdr(p_values: Iterable[float], q: float = 0.10) -> list[dict]:
    """Benjamini-Hochberg FDR correction. Returns per-input dicts in input order:
        {"p_raw", "p_adj", "rejected"}.
    Use for exploratory cells where FWER is too aggressive (Demšar 2006; BH 1995).
    """
    ps = list(p_values)
    n = len(ps)
    if n == 0:
        return []
    order_desc = sorted(range(n), key=lambda i: ps[i], reverse=True)
    p_adj = [0.0] * n
    # Step-up: walk from largest to smallest, maintain running min of p*(n/rank).
    running_min = 1.0
    for rev_rank, idx in enumerate(order_desc):
        rank = n - rev_rank
        adj = ps[idx] * n / rank
        if adj > 1.0:
            adj = 1.0
        running_min = min(running_min, adj)
        p_adj[idx] = running_min
    return [{"p_raw": ps[i], "p_adj": p_adj[i], "rejected": p_adj[i] < q} for i in range(n)]


def friedman_nemenyi(scores: dict[str, list[float]]) -> dict:
    """Friedman omnibus test + Nemenyi post-hoc for comparing k methods over n
    paired observations (e.g., 4 scoring modes across the same instances).

    Input: dict method_name -> list of per-instance scores (all lists same length).
    Returns:
        {
          "friedman": {"chi2", "p_value"},
          "mean_ranks": dict method -> mean rank,
          "critical_difference": float (Nemenyi CD at alpha=0.05),
          "pairwise": list of {"a","b","rank_diff","reject_at_0.05"}
        }

    Reference: Demšar (2006) JMLR 7:1-30. Standard for ML-benchmark
    multi-method, multi-dataset comparisons.
    """
    methods = list(scores.keys())
    k = len(methods)
    if k < 3:
        raise ValueError(f"Friedman/Nemenyi needs k>=3 methods, got {k}")
    arrs = [np.asarray(scores[m], dtype=np.float64) for m in methods]
    n = len(arrs[0])
    if any(len(a) != n for a in arrs):
        raise ValueError("all method score lists must be same length (paired observations)")
    if n < 2:
        raise ValueError(f"need >=2 paired observations, got {n}")

    from scipy.stats import friedmanchisquare, rankdata

    chi2, p = friedmanchisquare(*arrs)

    # Per-instance ranks (1 = best = highest score; we negate so rankdata's
    # ascending becomes descending). Average ranks across ties.
    matrix = np.stack(arrs, axis=0)  # shape (k, n)
    ranks = np.empty_like(matrix)
    for j in range(n):
        ranks[:, j] = rankdata(-matrix[:, j], method="average")
    mean_ranks = ranks.mean(axis=1)

    # Nemenyi critical difference at alpha=0.05.
    # Studentized-range q values (k=2..10) at alpha=0.05 (Demsar Table 5).
    q_alpha_05 = {2: 1.960, 3: 2.343, 4: 2.569, 5: 2.728, 6: 2.850, 7: 2.949, 8: 3.031, 9: 3.102, 10: 3.164}
    q = q_alpha_05.get(k, 3.164)
    cd = q * float(np.sqrt(k * (k + 1) / (6.0 * n)))

    pairwise = []
    for i in range(k):
        for j in range(i + 1, k):
            diff = abs(float(mean_ranks[i] - mean_ranks[j]))
            pairwise.append(
                {
                    "a": methods[i],
                    "b": methods[j],
                    "rank_diff": diff,
                    "reject_at_0.05": diff > cd,
                }
            )

    return {
        "friedman": {"chi2": float(chi2), "p_value": float(p)},
        "mean_ranks": {methods[i]: float(mean_ranks[i]) for i in range(k)},
        "critical_difference": cd,
        "pairwise": pairwise,
    }


def stouffer_combine(p_values: Iterable[float], weights: Iterable[float] | None = None) -> dict:
    """Stouffer's Z-method to combine independent one-sided p-values across
    test sets into a single pooled p-value. Used for the headline P1 test
    (diffctx vs BM25 across 3 benchmarks).
    """
    from scipy.stats import norm

    ps = np.asarray(list(p_values), dtype=np.float64)
    if len(ps) == 0:
        return {"z": 0.0, "p_value": 1.0}
    # Avoid log(0)/inf from p_value=0 by clamping.
    ps = np.clip(ps, 1e-300, 1.0)
    z = norm.isf(ps)  # one-sided z = Phi^{-1}(1 - p)
    if weights is not None:
        w = np.asarray(list(weights), dtype=np.float64)
        z_combined = float(np.sum(w * z) / np.sqrt(np.sum(w * w)))
    else:
        z_combined = float(np.sum(z) / np.sqrt(len(z)))
    p_combined = float(norm.sf(z_combined))
    return {"z": z_combined, "p_value": p_combined}
