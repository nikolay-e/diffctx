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


def tost_paired(
    before: list[float],
    after: list[float],
    margin: float = 0.02,
    n_iter: int = 10000,
    seed: int = 42,
    alpha: float = 0.05,
) -> dict:
    """Two One-Sided Tests on the paired delta `after - before`.

    Equivalence is declared at level alpha iff both one-sided tests reject:
    `P(boot_delta ≤ -margin) < alpha` and `P(boot_delta ≥ +margin) < alpha`.

    HARK warning: `margin` MUST be pre-registered before looking at the data.
    Default 0.02 (=2pp recall) is the smallest practically meaningful gap in
    information-retrieval evaluation per Sakai/Smucker conventions, but
    using it post-hoc on already-collected data is invalid (Lakens 2017).
    """
    if not before or not after or len(before) != len(after):
        return {
            "delta_mean": 0.0,
            "p_lower": float("nan"),
            "p_upper": float("nan"),
            "equivalent": False,
            "note": "empty/mismatched",
        }
    b = np.asarray(before, dtype=np.float64)
    a = np.asarray(after, dtype=np.float64)
    diffs = a - b
    if len(diffs) < 6:
        return {
            "delta_mean": float(diffs.mean()),
            "p_lower": float("nan"),
            "p_upper": float("nan"),
            "equivalent": False,
            "note": f"n={len(diffs)} too small for TOST",
        }
    rng = np.random.default_rng(seed)
    samples = rng.choice(diffs, size=(n_iter, len(diffs)), replace=True)
    boot = samples.mean(axis=1)
    p_lower = max(float((boot <= -margin).mean()), 1.0 / n_iter)
    p_upper = max(float((boot >= +margin).mean()), 1.0 / n_iter)
    return {
        "delta_mean": float(diffs.mean()),
        "margin": margin,
        "p_lower": p_lower,
        "p_upper": p_upper,
        "equivalent": (p_lower < alpha) and (p_upper < alpha),
    }


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
