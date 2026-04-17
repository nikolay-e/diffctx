from __future__ import annotations

import numpy as np


def bootstrap_ci(values: list[float], n_iter: int = 10000, alpha: float = 0.05, seed: int = 42) -> tuple[float, float, float]:
    if not values:
        return (0.0, 0.0, 0.0)
    arr = np.array(values, dtype=np.float64)
    if len(arr) == 1:
        return (float(arr[0]), float(arr[0]), float(arr[0]))
    rng = np.random.default_rng(seed)
    means = np.array([rng.choice(arr, size=len(arr), replace=True).mean() for _ in range(n_iter)])
    lo = float(np.percentile(means, 100 * alpha / 2))
    hi = float(np.percentile(means, 100 * (1 - alpha / 2)))
    return (float(arr.mean()), lo, hi)


def paired_bootstrap_delta(before: list[float], after: list[float], n_iter: int = 10000, seed: int = 42) -> dict:
    if not before or not after or len(before) != len(after):
        return {"delta_mean": 0.0, "ci_lo": 0.0, "ci_hi": 0.0, "p_value": 1.0}
    b = np.array(before, dtype=np.float64)
    a = np.array(after, dtype=np.float64)
    diffs = a - b
    if len(diffs) == 1:
        d = float(diffs[0])
        return {"delta_mean": d, "ci_lo": d, "ci_hi": d, "p_value": 0.0 if d > 0 else 1.0}
    rng = np.random.default_rng(seed)
    boot_deltas = np.array([rng.choice(diffs, size=len(diffs), replace=True).mean() for _ in range(n_iter)])
    return {
        "delta_mean": float(diffs.mean()),
        "ci_lo": float(np.percentile(boot_deltas, 2.5)),
        "ci_hi": float(np.percentile(boot_deltas, 97.5)),
        "p_value": float((boot_deltas <= 0).mean()),
    }


def wilcoxon_paired(before: list[float], after: list[float]) -> dict:
    if not before or not after or len(before) != len(after):
        return {"statistic": 0.0, "p_value": 1.0}
    b = np.array(before, dtype=np.float64)
    a = np.array(after, dtype=np.float64)
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
