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
    """Paired bootstrap on the per-instance delta `after - before`.

    Returns `delta_mean`, 95% CI bounds, and a one-sided p-value
    `P(delta_mean ≤ 0 | bootstrap)`. The p-value is clamped to `[1/n_iter, 1]`
    — exactly 0 cannot be observed (the smallest tail mass is one resample),
    and rendering literal 0 is misleading. Single-pair calls return NaN p
    so downstream multiple-testing corrections can drop them.
    """
    if not before or not after or len(before) != len(after):
        return {"delta_mean": 0.0, "ci_lo": 0.0, "ci_hi": 0.0, "p_value": 1.0}
    b = np.asarray(before, dtype=np.float64)
    a = np.asarray(after, dtype=np.float64)
    diffs = a - b
    if len(diffs) == 1:
        d = float(diffs[0])
        return {"delta_mean": d, "ci_lo": d, "ci_hi": d, "p_value": float("nan")}
    rng = np.random.default_rng(seed)
    samples = rng.choice(diffs, size=(n_iter, len(diffs)), replace=True)
    boot_deltas = samples.mean(axis=1)
    p_raw = float((boot_deltas <= 0).mean())
    p_floor = 1.0 / n_iter
    return {
        "delta_mean": float(diffs.mean()),
        "ci_lo": float(np.percentile(boot_deltas, 2.5)),
        "ci_hi": float(np.percentile(boot_deltas, 97.5)),
        "p_value": max(p_raw, p_floor),
    }


def wilcoxon_paired(before: list[float], after: list[float]) -> dict:
    """Two-sided exact Wilcoxon signed-rank.

    Returns NaN p when fewer than 6 non-zero paired differences exist — the
    two-sided exact test cannot reach p<0.05 with n_nonzero < 6, so reporting
    a numeric value would be misleading (typically inflated to ≈1 by the
    default `zero_method='wilcox'` which drops zeros).
    """
    if not before or not after or len(before) != len(after):
        return {"statistic": float("nan"), "p_value": float("nan"), "note": "empty/mismatched"}
    b = np.asarray(before, dtype=np.float64)
    a = np.asarray(after, dtype=np.float64)
    diffs = a - b
    nonzero = int(np.count_nonzero(diffs))
    if nonzero < 6:
        return {"statistic": float("nan"), "p_value": float("nan"), "note": f"only {nonzero} nonzero diffs"}
    from scipy.stats import wilcoxon as _wilcoxon

    try:
        result = _wilcoxon(a, b)
        # scipy.stats.wilcoxon returns a namedtuple; access by index for stable typing.
        stat = float(result[0])  # type: ignore[index]
        p = float(result[1])  # type: ignore[index]
        return {"statistic": stat, "p_value": p}
    except ValueError:
        return {"statistic": float("nan"), "p_value": float("nan"), "note": "scipy raised ValueError"}


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
