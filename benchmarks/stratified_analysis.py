"""End-to-end stratified analysis of a sweep run.

Loads every per-instance checkpoint from a sweep directory, materializes a
long-form table, and emits paper-ready statistical artifacts:

  - bucketed recall (by |gold|, by difficulty ratio) with sample sizes and
    bootstrap CIs, both pooled and per-dataset
  - paired bootstrap CIs and Wilcoxon p-values for the key method pairs
    (EGO L=4 vs PPR, EGO L=4 vs EGO L=0, EGO L=2 vs L=4, EGO L=4 vs bm25 128k),
    Holm-corrected across the family
  - log-linear regression recall ~ log(1+ratio) + log(1+|gold|) per method,
    coefficients with bootstrap CIs
  - per-language recall in the hard regime (ratio>1.5)
  - matched-cardinality scan (fragment_count proxy)

CLI:
    python -m benchmarks.stratified_analysis \
        --cells-dir /tmp/sweep-25402041321 --out /tmp/sweep-25402041321/stratified
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from collections import defaultdict
from collections.abc import Iterable, Sequence
from pathlib import Path

import numpy as np

# Bucket constants live in cell_metrics — single source of truth so the
# per-cell aggregator and the cross-cell stratified analysis cannot drift.
from benchmarks.cell_metrics import _GOLD_BUCKETS, _RATIO_BUCKETS
from benchmarks.stats import bh_fdr, bootstrap_ci, holm_correct, paired_bootstrap_delta, wilcoxon_paired

# ---------------------------------------------------------------------------
# Long-form loader
# ---------------------------------------------------------------------------


def load_long(cells_dir: Path) -> list[dict]:
    """Walk every cell-* checkpoint and emit one row per (instance, cell)."""
    rows: list[dict] = []
    for cell in sorted(cells_dir.iterdir()):
        if not cell.is_dir() or not cell.name.startswith("cell-"):
            continue
        meta_path = cell / "metadata.json"
        if not meta_path.exists():
            continue
        try:
            meta = json.loads(meta_path.read_text())
        except (ValueError, OSError):
            continue
        cell_info = meta.get("cell") or {}
        method = cell_info.get("method")
        budget = cell_info.get("budget")
        depth = cell_info.get("depth")
        test_set = cell_info.get("test_set")
        if not method or budget is None or test_set is None:
            continue
        ckpts = list(cell.glob("*.checkpoint.jsonl"))
        if not ckpts:
            continue
        with ckpts[0].open() as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    r = json.loads(line)
                except ValueError:
                    continue
                ex = r.get("extra") or {}
                rows.append(
                    {
                        "instance_id": r.get("instance_id"),
                        "dataset": test_set,
                        "method": method,
                        "budget": int(budget),
                        "depth": int(depth) if depth is not None else -1,
                        "file_recall": float(r.get("file_recall") or 0.0),
                        "file_precision": float(r.get("file_precision") or 0.0),
                        "used_tokens": int(r.get("used_tokens") or 0),
                        "elapsed_seconds": float(r.get("elapsed_seconds") or 0.0),
                        "language": str(ex.get("language") or "unknown"),
                        "n_gold": int(ex.get("n_gold") or 0),
                        "n_changed_files": int(ex.get("n_changed_files") or 0),
                        "diff_size_lines": int(ex.get("diff_size_lines") or 0),
                        "gold_to_changed_ratio": float(ex.get("gold_to_changed_ratio") or 0.0),
                        "fragment_count": int(ex.get("fragment_count") or 0),
                        "n_selected": int(ex.get("n_selected") or ex.get("fragment_count") or 0),
                        "status": str(ex.get("status") or "missing"),
                    }
                )
    return rows


def long_to_arrays(rows: list[dict]) -> dict[tuple[str, int, int, str], dict[str, np.ndarray]]:
    """Pivot to {(method, budget, depth, dataset) → {field → np.ndarray}}."""
    grouped: dict[tuple[str, int, int, str], list[dict]] = defaultdict(list)
    for r in rows:
        grouped[(r["method"], r["budget"], r["depth"], r["dataset"])].append(r)
    out: dict[tuple[str, int, int, str], dict[str, np.ndarray]] = {}
    fields = (
        "instance_id",
        "file_recall",
        "file_precision",
        "used_tokens",
        "elapsed_seconds",
        "language",
        "n_gold",
        "gold_to_changed_ratio",
        "fragment_count",
        "n_selected",
        "status",
        "diff_size_lines",
    )
    for key, rs in grouped.items():
        out[key] = {
            f: np.asarray([r[f] for r in rs], dtype=object if f in ("instance_id", "language", "status") else float)
            for f in fields
        }
    return out


# ---------------------------------------------------------------------------
# Bucketing helpers
# ---------------------------------------------------------------------------


def _bucket_by_ratio(values: np.ndarray) -> dict[str, np.ndarray]:
    out: dict[str, np.ndarray] = {}
    for label, lo, hi in _RATIO_BUCKETS:
        mask = values >= lo
        if hi is not None:
            mask &= values < hi  # half-open [lo, hi)
        out[label] = mask
    return out


def _bucket_by_gold(values: np.ndarray) -> dict[str, np.ndarray]:
    out: dict[str, np.ndarray] = {}
    for label, lo, hi in _GOLD_BUCKETS:
        mask = values >= lo
        if hi is not None:
            mask &= values < hi  # half-open [lo, hi)
        out[label] = mask
    return out


# ---------------------------------------------------------------------------
# Per-bucket recall with bootstrap CI
# ---------------------------------------------------------------------------


def per_bucket_recall(
    by_cell: dict[tuple[str, int, int, str], dict[str, np.ndarray]],
    bucket_field: str,
    bucket_fn,
) -> list[dict]:
    """For each (method, budget, depth, dataset, bucket): n + mean recall + bootstrap CI."""
    out: list[dict] = []
    for (method, budget, depth, dataset), arrs in by_cell.items():
        masks = bucket_fn(arrs[bucket_field])
        for label, mask in masks.items():
            n = int(mask.sum())
            if n == 0:
                continue
            sample = arrs["file_recall"][mask].astype(float).tolist()
            mean, lo, hi = bootstrap_ci(sample, n_iter=2000)
            out.append(
                {
                    "method": method,
                    "budget": budget,
                    "depth": depth,
                    "dataset": dataset,
                    "bucket": label,
                    "n": n,
                    "mean_recall": mean,
                    "ci_lo": lo,
                    "ci_hi": hi,
                }
            )
    return out


# ---------------------------------------------------------------------------
# Paired comparisons
# ---------------------------------------------------------------------------


def _paired_pull(
    cell_a: dict[str, np.ndarray],
    cell_b: dict[str, np.ndarray],
    bucket_label: str,
    bucket_field: str,
    bucket_fn,
) -> tuple[list[float], list[float], int, int, int]:
    """Return paired recalls and the (n_a_in_bucket, n_b_in_bucket, n_paired) tuple.

    `n_paired` is the cardinality after instance-id intersection — what the
    bootstrap actually sees. Exposing all three lets the renderer flag cells
    where one side has more bucket members than the other (coverage gap).
    """
    ids_a = {iid: idx for idx, iid in enumerate(cell_a["instance_id"])}
    masks_a = bucket_fn(cell_a[bucket_field])
    masks_b = bucket_fn(cell_b[bucket_field])
    mask_a = masks_a[bucket_label]
    n_a = int(mask_a.sum())
    n_b = int(masks_b[bucket_label].sum())
    a_vals: list[float] = []
    b_vals: list[float] = []
    for idx_b, iid in enumerate(cell_b["instance_id"]):
        if iid not in ids_a:
            continue
        idx_a = ids_a[iid]
        if not mask_a[idx_a]:
            continue
        a_vals.append(float(cell_a["file_recall"][idx_a]))
        b_vals.append(float(cell_b["file_recall"][idx_b]))
    return a_vals, b_vals, n_a, n_b, len(a_vals)


def pairwise_comparisons(
    by_cell: dict[tuple[str, int, int, str], dict[str, np.ndarray]],
    pairs: Sequence[tuple[tuple[str, int, int], tuple[str, int, int], str, str, str]],
    datasets: Iterable[str],
    bucket_field: str,
    bucket_fn,
) -> list[dict]:
    """Paired bootstrap + Wilcoxon for each (pair, dataset, bucket).

    Each pair entry is `(a_cfg, b_cfg, label, claim_id, correction)`. Multiple-
    testing correction is applied **within `claim_id`** (Bender & Lange 2001:
    the family is defined by the conclusion, not the test count). Pairs marked
    `correction='holm'` use FWER control; pairs marked `'fdr'` use BH-FDR
    (treated as exploratory).

    Cells with `n_paired < 6` are skipped (the two-sided Wilcoxon cannot
    reach p<0.05 below that). `mean_a_marginal` / `mean_b_marginal` are the
    cell averages on the paired subset and are diagnostic only — the
    HEADLINE Δ MUST be `delta` (paired). Renderers must surface that
    distinction.
    """
    out: list[dict] = []
    for a_cfg, b_cfg, label, claim_id, correction in pairs:
        for ds in datasets:
            cell_a = by_cell.get((*a_cfg, ds))
            cell_b = by_cell.get((*b_cfg, ds))
            if cell_a is None or cell_b is None:
                continue
            bucket_defs = _RATIO_BUCKETS if bucket_field == "gold_to_changed_ratio" else _GOLD_BUCKETS
            for bucket_def in bucket_defs:
                bucket_label = bucket_def[0]
                a_vals, b_vals, n_a, n_b, n_paired = _paired_pull(cell_a, cell_b, bucket_label, bucket_field, bucket_fn)
                if n_paired < 6:
                    continue
                boot = paired_bootstrap_delta(a_vals, b_vals, n_iter=5000)
                wilc = wilcoxon_paired(a_vals, b_vals)
                out.append(
                    {
                        "claim_id": claim_id,
                        "correction": correction,
                        "pair_label": label,
                        "a": f"{a_cfg[0]} b={a_cfg[1]} L={a_cfg[2]}",
                        "b": f"{b_cfg[0]} b={b_cfg[1]} L={b_cfg[2]}",
                        "dataset": ds,
                        "bucket": bucket_label,
                        "n_a": n_a,
                        "n_b": n_b,
                        "n_paired": n_paired,
                        # Cell-mean for diagnostic only; HEADLINE Δ must be `delta` (paired).
                        "mean_a_marginal": float(np.mean(a_vals)),
                        "mean_b_marginal": float(np.mean(b_vals)),
                        "delta": boot["delta_mean"],
                        "ci_lo": boot["ci_lo"],
                        "ci_hi": boot["ci_hi"],
                        "p_boot": boot["p_value"],
                        "p_wilcoxon": wilc["p_value"],
                    }
                )
    # Apply correction within each (claim_id, correction) family separately.
    by_claim: dict[tuple[str, str], list[int]] = defaultdict(list)
    for i, r in enumerate(out):
        by_claim[(r["claim_id"], r["correction"])].append(i)
    for (_claim_id, correction), idxs in by_claim.items():
        ps = [out[i]["p_boot"] for i in idxs]
        corrected = bh_fdr(ps, q=0.10) if correction == "fdr" else holm_correct(ps)
        for i, c in zip(idxs, corrected):
            out[i]["p_adj"] = c["p_adj"]
            out[i]["adj_reject"] = c["rejected"]
            out[i]["family_size"] = len(idxs)
    return out


# ---------------------------------------------------------------------------
# Continuous regression
# ---------------------------------------------------------------------------


def _fit_regression_with_cluster_bootstrap(
    recall: np.ndarray,
    ratio: np.ndarray,
    n_gold: np.ndarray,
    instance_ids: np.ndarray,
    n_boot: int = 1000,
    seed: int = 42,
) -> dict | None:
    """OLS `recall ~ 1 + log1p(ratio) + log1p(n_gold)` with cluster bootstrap on instance_id.

    Returns None if `var(log1p(ratio)) < 1e-6` (degenerate slope) or the
    point fit is rank-deficient. Failing bootstrap resamples (rank-deficient
    matrix) record NaN for the coefficient vector, and CIs are computed via
    `np.nanpercentile` so partial degeneracy doesn't pull CIs toward the
    point estimate.
    """
    if len(recall) < 30:
        return None
    x_ratio = np.log1p(ratio)
    x_gold = np.log1p(n_gold)
    if np.var(x_ratio) < 1e-6:
        return None
    x = np.column_stack([np.ones_like(recall), x_ratio, x_gold])
    try:
        coef = np.linalg.lstsq(x, recall, rcond=None)[0]
    except np.linalg.LinAlgError:
        return None
    if not np.all(np.isfinite(coef)):
        return None

    # Cluster on unique instance_ids: resample clusters with replacement, take
    # all rows in chosen clusters. This respects within-instance correlation
    # (multiple budget/depth cells observe the same instance) without treating
    # methods as exchangeable — this regression is fit on a single (method,
    # budget, depth, dataset) cell so each instance contributes ≤1 row anyway,
    # but clustering is still the principled bootstrap unit per IR convention.
    cluster_ids, cluster_index = np.unique(instance_ids, return_inverse=True)
    rng = np.random.default_rng(seed)
    boot_coefs = np.full((n_boot, 3), np.nan, dtype=np.float64)
    n_clusters = len(cluster_ids)
    for i in range(n_boot):
        chosen = rng.integers(0, n_clusters, n_clusters)
        mask = np.isin(cluster_index, chosen)
        if not mask.any():
            continue
        x_b = x[mask]
        y_b = recall[mask]
        if x_b.shape[0] < 4 or np.linalg.matrix_rank(x_b) < x_b.shape[1]:
            continue
        try:
            boot_coefs[i] = np.linalg.lstsq(x_b, y_b, rcond=None)[0]
        except np.linalg.LinAlgError:
            continue
    n_valid = int(np.sum(np.isfinite(boot_coefs[:, 0])))
    if n_valid < 100:
        return None
    ci_lo = np.nanpercentile(boot_coefs, 2.5, axis=0)
    ci_hi = np.nanpercentile(boot_coefs, 97.5, axis=0)
    return {
        "n": len(recall),
        "n_clusters": int(n_clusters),
        "n_boot_valid": n_valid,
        "intercept": float(coef[0]),
        "intercept_ci": (float(ci_lo[0]), float(ci_hi[0])),
        "ratio_slope": float(coef[1]),
        "ratio_slope_ci": (float(ci_lo[1]), float(ci_hi[1])),
        "gold_slope": float(coef[2]),
        "gold_slope_ci": (float(ci_lo[2]), float(ci_hi[2])),
    }


def regression_per_method(
    by_cell: dict[tuple[str, int, int, str], dict[str, np.ndarray]],
) -> list[dict]:
    """Per-(method, budget, depth, dataset) OLS with cluster-bootstrap CIs.

    Pooled-across-datasets regression conflates Simpson-style between-dataset
    mean shifts with within-dataset slope. We fit per-dataset and let the
    reader compare. Datasets with ≤1e-6 variance in log1p(ratio) are skipped
    automatically (e.g. swebench has ratio≡1 → no slope to fit).
    """
    out: list[dict] = []
    for cell_key, arrs in by_cell.items():
        method, budget, depth, dataset = cell_key
        if budget == 0:
            continue
        fit = _fit_regression_with_cluster_bootstrap(
            arrs["file_recall"].astype(float),
            arrs["gold_to_changed_ratio"].astype(float),
            arrs["n_gold"].astype(float),
            np.asarray(arrs["instance_id"]),
        )
        if fit is None:
            continue
        out.append({"method": method, "budget": budget, "depth": depth, "dataset": dataset, **fit})
    return out


# ---------------------------------------------------------------------------
# Per-language hard regime (ratio>1.5)
# ---------------------------------------------------------------------------


def per_language_hard_regime(by_cell: dict[tuple[str, int, int, str], dict[str, np.ndarray]]) -> list[dict]:
    """Per-language recall in the *hard* regime, defined as ratio in the
    `1.5-2.0` bucket or higher (i.e. matches `_RATIO_BUCKETS[2:]`). Threshold
    is taken from the bucket constants, not hard-coded, so it cannot drift."""
    hard_bucket_lo = _RATIO_BUCKETS[2][1]  # lower bound of "1.5-2.0" bucket
    out: list[dict] = []
    for (method, budget, depth, dataset), arrs in by_cell.items():
        if budget == 0:
            continue
        mask = arrs["gold_to_changed_ratio"] >= hard_bucket_lo
        if not mask.any():
            continue
        languages = arrs["language"][mask]
        recalls = arrs["file_recall"][mask].astype(float)
        for lang in np.unique(languages):
            lang_mask = languages == lang
            n = int(lang_mask.sum())
            if n < 5:
                continue
            sample = recalls[lang_mask].tolist()
            mean, lo, hi = bootstrap_ci(sample, n_iter=2000)
            out.append(
                {
                    "method": method,
                    "budget": budget,
                    "depth": depth,
                    "dataset": dataset,
                    "language": str(lang),
                    "n": n,
                    "mean_recall": mean,
                    "ci_lo": lo,
                    "ci_hi": hi,
                }
            )
    return out


# ---------------------------------------------------------------------------
# Matched cardinality (fragment_count proxy)
# ---------------------------------------------------------------------------


def matched_cardinality_scan(
    by_cell: dict[tuple[str, int, int, str], dict[str, np.ndarray]],
) -> list[dict]:
    """For each (method, budget, depth, dataset) report median fragment_count + mean recall.

    Caller groups by similar fragment_count to compare apples-to-apples.
    """
    out: list[dict] = []
    for (method, budget, depth, dataset), arrs in by_cell.items():
        if budget == 0:
            continue
        recalls = arrs["file_recall"].astype(float)
        precs = arrs["file_precision"].astype(float)
        cards = arrs["fragment_count"].astype(float)
        mean, lo, hi = bootstrap_ci(recalls.tolist(), n_iter=2000)
        out.append(
            {
                "method": method,
                "budget": budget,
                "depth": depth,
                "dataset": dataset,
                "n": len(recalls),
                "median_cardinality": float(np.median(cards)),
                "mean_cardinality": float(np.mean(cards)),
                "mean_recall": mean,
                "ci_lo": lo,
                "ci_hi": hi,
                "mean_precision": float(np.mean(precs)),
            }
        )
    return out


# ---------------------------------------------------------------------------
# Rendering
# ---------------------------------------------------------------------------


def _fmt_recall_ci(rec: dict) -> str:
    """Render `mean [CI_lo, CI_hi] (n)`, flagging degenerate cells with `‡`.

    A CI half-width below 1e-12 means the per-instance recall is constant
    across the bucket (e.g. SWE-bench saturated cell where every instance
    returns recall=1.0). Distinguishing these from honest narrow CIs avoids
    misreading a constant as a tightly estimated value.
    """
    hw = (rec["ci_hi"] - rec["ci_lo"]) / 2
    flag = "‡" if hw < 1e-12 else ""
    return f"{rec['mean_recall']:.3f}{flag} [{rec['ci_lo']:.3f}, {rec['ci_hi']:.3f}] (n={rec['n']})"


def render_per_bucket_table(rows: list[dict], buckets: Sequence[str], title: str) -> str:
    """Per-bucket recall with CI, one row per (method, budget, depth, dataset)."""
    grouped: dict[tuple[str, int, int, str], dict[str, dict]] = defaultdict(dict)
    for r in rows:
        grouped[(r["method"], r["budget"], r["depth"], r["dataset"])][r["bucket"]] = r
    if not grouped:
        return ""
    out: list[str] = [f"\n## {title}", ""]
    out.append("Each cell shows: `mean [95% bootstrap CI] (n)`. CI uses 2000 resamples.")
    out.append("")
    out.append("| method | budget | depth | dataset | " + " | ".join(buckets) + " |")
    out.append("|---|---:|---:|---|" + "---|" * len(buckets))
    method_order = ["aider", "bm25", "ppr", "ego"]
    keys = sorted(
        grouped.keys(),
        key=lambda k: (method_order.index(k[0]) if k[0] in method_order else 99, k[1], k[2], k[3]),
    )
    for key in keys:
        cells = []
        for b in buckets:
            r = grouped[key].get(b)
            cells.append(_fmt_recall_ci(r) if r else "—")
        out.append(f"| **{key[0]}** | {key[1]} | {key[2]} | {key[3]} | " + " | ".join(cells) + " |")
    return "\n".join(out) + "\n"


def render_pooled_per_bucket_table(rows: list[dict], buckets: Sequence[str], title: str) -> str:
    """Pool over datasets, weighted by n.

    **Caveat surfaced explicitly per cell:** for buckets where only one
    dataset contributes (e.g. ratio>1.0 is ContextBench-only because SWE-V
    and PolyBench500 have ratio≡1 by construction), the cell is annotated
    with the contributing dataset list. Without this annotation, "(pooled
    across datasets)" is misleading framing.
    """
    pooled: dict[tuple[str, int, int], dict[str, list[dict]]] = defaultdict(lambda: defaultdict(list))
    for r in rows:
        pooled[(r["method"], r["budget"], r["depth"])][r["bucket"]].append(r)
    # Per-bucket coverage map: which datasets contributed at all (for any cfg)?
    contributing: dict[str, set[str]] = defaultdict(set)
    for r in rows:
        contributing[r["bucket"]].add(r["dataset"])

    out: list[str] = [f"\n## {title} (pooled across datasets)", ""]
    out.append(
        "Pooled means weighted by per-dataset n. CI is half-width approximation "
        "`sqrt(Σnᵢ·varᵢ)/Σnᵢ`. Bucket header notes the *contributing* datasets — "
        "for buckets where only one of {swebench, polybench, contextbench} has "
        "any instances, this is **NOT** a cross-dataset pool but a single-dataset "
        "result mislabelled by the convenience of the matrix shape."
    )
    out.append("")
    bucket_headers: list[str] = []
    for b in buckets:
        ds = sorted(contributing.get(b, set()))
        if not ds:
            bucket_headers.append(b)
        elif len(ds) == 1:
            bucket_headers.append(f"{b} (**{ds[0]} only**)")
        else:
            short = ",".join(d[:4] for d in ds)
            bucket_headers.append(f"{b} ({short})")
    out.append("| method | budget | depth | " + " | ".join(bucket_headers) + " |")
    out.append("|---|---:|---:|" + "---|" * len(buckets))
    method_order = ["aider", "bm25", "ppr", "ego"]
    keys = sorted(
        pooled.keys(),
        key=lambda k: (method_order.index(k[0]) if k[0] in method_order else 99, k[1], k[2]),
    )
    for key in keys:
        cells = []
        for b in buckets:
            cell_rows = pooled[key].get(b, [])
            if not cell_rows:
                cells.append("—")
                continue
            total_n = sum(r["n"] for r in cell_rows)
            mean = sum(r["mean_recall"] * r["n"] for r in cell_rows) / total_n
            # half-widths combined as if independent (conservative for small n)
            hw = sum((r["ci_hi"] - r["ci_lo"]) / 2 * r["n"] for r in cell_rows) / total_n
            cells.append(f"{mean:.3f} ±{hw:.3f} (n={total_n})")
        out.append(f"| **{key[0]}** | {key[1]} | {key[2]} | " + " | ".join(cells) + " |")
    return "\n".join(out) + "\n"


def _fmt_p(p: float) -> str:
    """Render a probability for the markdown table; tiny values clamp to `<1e-10`."""
    if math.isnan(p):
        return "n/a"
    if p < 1e-10:
        return "<1e-10"
    return f"{p:.3g}"


def render_pairwise_table(rows: list[dict], title: str) -> str:
    if not rows:
        return ""
    out: list[str] = [f"\n## {title}", ""]
    out.append(
        "**Δ is paired** (instance-id matched, computed on the same instances "
        "for both methods). 95% bootstrap CI from 5000 resamples on the "
        "per-instance Δ; rendered `<1e-10` when the bootstrap floor (1/n_iter) "
        "is hit. The `mean_a / mean_b` columns are the *marginal* per-method "
        "means on the paired subset and are diagnostic only — **the headline "
        "effect is `Δ`, not `mean_b - mean_a`**, because group means do not "
        "control for shared instance difficulty (Smucker, Allan, Carterette, "
        "CIKM 2007). Multiple-testing correction is applied **within each "
        "`claim_id` family**: pre-registered claims use Holm-Bonferroni "
        "(FWER), exploratory claims use BH-FDR (q=0.10)."
    )
    out.append("")
    out.append(
        "| claim_id (correction) | pair | dataset | bucket | n_paired | "
        "mean_a (marg) | mean_b (marg) | Δ paired | 95% CI | Wilcoxon p | "
        "adj p | reject? | family |"
    )
    out.append("|---|---|---|---|---:|---:|---:|---:|---|---:|---:|---|---:|")
    correction_order = {"holm": 0, "fdr": 1}
    rows_sorted = sorted(
        rows,
        key=lambda r: (
            r.get("claim_id", ""),
            correction_order.get(r.get("correction", ""), 99),
            r.get("pair_label", ""),
            r.get("dataset", ""),
            r.get("bucket", ""),
        ),
    )
    for r in rows_sorted:
        ci = f"[{r['ci_lo']:+.3f}, {r['ci_hi']:+.3f}]"
        reject = "✓" if r.get("adj_reject") else "—"
        out.append(
            f"| **{r.get('claim_id', '?')}** ({r.get('correction', '?')}) | "
            f"{r['pair_label']} | {r['dataset']} | {r['bucket']} | "
            f"{r['n_paired']} | {r['mean_a_marginal']:.3f} | {r['mean_b_marginal']:.3f} | "
            f"{r['delta']:+.3f} | {ci} | "
            f"{_fmt_p(r['p_wilcoxon'])} | {_fmt_p(r['p_adj'])} | {reject} | "
            f"{r.get('family_size', '?')} |"
        )
    return "\n".join(out) + "\n"


def render_regression_table(rows: list[dict]) -> str:
    if not rows:
        return ""
    out: list[str] = ["\n## Continuous regression: recall ~ log(1+ratio) + log(1+|gold|)", ""]
    out.append(
        "**Per-(method, budget, depth, dataset)** OLS with cluster bootstrap on `instance_id`. "
        "Pooled-across-datasets regression is omitted: it would mix Simpson-style between-dataset "
        "mean shifts with within-dataset difficulty slopes (Cañamares & Castells, CIKM 2021). "
        "Cells where `var(log1p(ratio)) < 1e-6` (e.g. swebench: every instance has ratio=1) are "
        "skipped — no slope is fittable when the regressor is constant."
    )
    out.append("")
    out.append("Larger (less negative) `ratio_slope` = better scaling on hard instances.")
    out.append("")
    out.append("| method | budget | depth | dataset | n | clusters | intercept | ratio_slope | ratio CI | gold_slope | gold CI |")
    out.append("|---|---:|---:|---|---:|---:|---:|---:|---|---:|---|")
    method_order = ["aider", "bm25", "ppr", "ego"]
    rows_sorted = sorted(
        rows,
        key=lambda r: (
            method_order.index(r["method"]) if r["method"] in method_order else 99,
            r["budget"],
            r["depth"],
            r["dataset"],
        ),
    )
    for r in rows_sorted:
        ratio_ci = f"[{r['ratio_slope_ci'][0]:+.3f}, {r['ratio_slope_ci'][1]:+.3f}]"
        gold_ci = f"[{r['gold_slope_ci'][0]:+.3f}, {r['gold_slope_ci'][1]:+.3f}]"
        out.append(
            f"| **{r['method']}** | {r['budget']} | {r['depth']} | {r['dataset']} | "
            f"{r['n']} | {r['n_clusters']} | "
            f"{r['intercept']:+.3f} | {r['ratio_slope']:+.3f} | {ratio_ci} | "
            f"{r['gold_slope']:+.3f} | {gold_ci} |"
        )
    return "\n".join(out) + "\n"


def render_per_language_hard(rows: list[dict]) -> str:
    if not rows:
        return ""
    by_method: dict[tuple[str, int, int, str], dict[str, dict]] = defaultdict(dict)
    languages: set[str] = set()
    for r in rows:
        by_method[(r["method"], r["budget"], r["depth"], r["dataset"])][r["language"]] = r
        languages.add(r["language"])
    if not languages:
        return ""

    out: list[str] = ["\n## Hard regime (ratio>1.5): recall by language", ""]
    out.append("Per-cell x per-language `mean [CI] (n)`. Languages with <5 instances per cell are dropped.")
    out.append("")
    method_order = ["aider", "bm25", "ppr", "ego"]
    keys = sorted(
        by_method.keys(),
        key=lambda k: (method_order.index(k[0]) if k[0] in method_order else 99, k[1], k[2], k[3]),
    )
    lang_order = sorted(languages)
    out.append("| method | budget | depth | dataset | " + " | ".join(lang_order) + " |")
    out.append("|---|---:|---:|---|" + "---|" * len(lang_order))
    for key in keys:
        cells = []
        for lang in lang_order:
            r = by_method[key].get(lang)
            cells.append(_fmt_recall_ci(r) if r else "—")
        out.append(f"| **{key[0]}** | {key[1]} | {key[2]} | {key[3]} | " + " | ".join(cells) + " |")
    return "\n".join(out) + "\n"


def render_matched_cardinality(rows: list[dict]) -> str:
    if not rows:
        return ""
    out: list[str] = ["\n## Cardinality scan: median fragments returned vs recall", ""]
    out.append(
        "Methods returning similar median cardinality should be compared head-to-head. Recall ± half-CI; precision is mean."
    )
    out.append("")
    out.append("| method | budget | depth | dataset | median_card | mean_card | recall | recall CI | precision |")
    out.append("|---|---:|---:|---|---:|---:|---:|---|---:|")
    method_order = ["aider", "bm25", "ppr", "ego"]
    rows_sorted = sorted(
        rows,
        key=lambda r: (
            method_order.index(r["method"]) if r["method"] in method_order else 99,
            r["budget"],
            r["depth"],
            r["dataset"],
        ),
    )
    for row in rows_sorted:
        ci = f"[{row['ci_lo']:.3f}, {row['ci_hi']:.3f}]"
        out.append(
            f"| **{row['method']}** | {row['budget']} | {row['depth']} | {row['dataset']} | "
            f"{row['median_cardinality']:.0f} | {row['mean_cardinality']:.1f} | "
            f"{row['mean_recall']:.3f} | {ci} | {row['mean_precision']:.3f} |"
        )
    return "\n".join(out) + "\n"


# ---------------------------------------------------------------------------
# Driver
# ---------------------------------------------------------------------------


def _render_structural_fact(rows: list[dict], _datasets: list[str]) -> str:
    """Lead block: per-dataset coverage of the hard regime (ratio>1.5).

    Surfaces the headline structural fact: only ContextBench has retrieval-
    meaningful instances; SWE-bench and PolyBench are saturated by
    construction (gold ⊆ diff). Without this, downstream pooled tables look
    cross-benchmark when they are de facto single-benchmark.
    """
    if not rows:
        return ""
    hard_lo = _RATIO_BUCKETS[2][1]
    by_ds: dict[str, dict[str, int]] = defaultdict(lambda: {"total": 0, "hard": 0, "trivial": 0})
    seen: set[tuple[str, str]] = set()
    for r in rows:
        # Each instance may appear in many cells; count it once per dataset.
        key = (r["dataset"], r["instance_id"])
        if key in seen:
            continue
        seen.add(key)
        ds = r["dataset"]
        by_ds[ds]["total"] += 1
        if r["gold_to_changed_ratio"] >= hard_lo:
            by_ds[ds]["hard"] += 1
        if r["gold_to_changed_ratio"] <= 1.0:
            by_ds[ds]["trivial"] += 1

    out: list[str] = ["## Structural fact (lead)", ""]
    out.append(
        "Only one of the three test sets has any instances where retrieval is "
        "non-trivial (`|gold|/|changed| > 1.5`). On the other two, gold ⊆ diff "
        "by construction, so every method recovers gold by returning the diff "
        "itself — the recall ceiling is identical and method ranking is "
        "uninformative. Pooling across all three dilutes any retrieval signal."
    )
    out.append("")
    out.append("| dataset | n | trivial (ratio≤1.0) | hard (ratio>1.5) | hard fraction |")
    out.append("|---|---:|---:|---:|---:|")
    total_n = 0
    total_hard = 0
    for ds in sorted(by_ds.keys()):
        info = by_ds[ds]
        n = info["total"]
        trivial = info["trivial"]
        hard = info["hard"]
        frac = hard / n if n else 0.0
        total_n += n
        total_hard += hard
        out.append(f"| {ds} | {n} | {trivial} | {hard} | {frac * 100:.1f}% |")
    if total_n:
        out.append(
            f"| **all** | **{total_n}** | "
            f"**{sum(d['trivial'] for d in by_ds.values())}** | "
            f"**{total_hard}** | **{total_hard / total_n * 100:.1f}%** |"
        )
    out.append("")
    out.append(
        "**Reading guide:** in tables below, the only cells that distinguish "
        "retrieval methods are the ones populated by datasets with hard "
        "instances. When a bucket header reads `(<dataset> only)` the row is "
        "single-dataset by structural necessity, not by analyst choice."
    )
    return "\n".join(out) + "\n"


def _key_pairs() -> list[tuple[tuple[str, int, int], tuple[str, int, int], str, str, str]]:
    """Headline method comparisons grouped into pre-registered claim families.

    Each entry: `(a_cfg, b_cfg, label, claim_id, correction)`. Δ in the table
    is `b - a` (positive = b wins). Multiple-testing correction is applied
    **within each `claim_id`**.

    Two pre-registered FWER-controlled claims:
      - "EGO_L4_vs_L0": does graph propagation (depth) help over no propagation?
      - "EGO_L4_vs_PPR": does our scoring beat the baseline at the same budget?
    Plus exploratory comparisons (BH-FDR):
      - "EGO_vs_BM25_LARGE": does EGO match brute-force lexical retrieval at 16x budget?
      - "EGO_INTERNAL_DEPTH": which depth is best (sweep)?
    """
    return [
        # Pre-registered claim 1: EGO L=4 vs no propagation (across budgets).
        (("ego", 8000, 0), ("ego", 8000, 4), "L=4 vs L=0 @ b=8000", "EGO_L4_vs_L0", "holm"),
        (("ego", 16000, 0), ("ego", 16000, 4), "L=4 vs L=0 @ b=16000", "EGO_L4_vs_L0", "holm"),
        (("ego", 32000, 0), ("ego", 32000, 4), "L=4 vs L=0 @ b=32000", "EGO_L4_vs_L0", "holm"),
        (("ego", -1, 0), ("ego", -1, 4), "L=4 vs L=0 @ unbudgeted", "EGO_L4_vs_L0", "holm"),
        # Pre-registered claim 2: EGO depth=4 vs PPR baseline (same budget).
        (("ppr", 8000, -1), ("ego", 8000, 4), "EGO L=4 vs PPR @ b=8000", "EGO_L4_vs_PPR", "holm"),
        (("ppr", 16000, -1), ("ego", 16000, 4), "EGO L=4 vs PPR @ b=16000", "EGO_L4_vs_PPR", "holm"),
        (("ppr", 32000, -1), ("ego", 32000, 4), "EGO L=4 vs PPR @ b=32000", "EGO_L4_vs_PPR", "holm"),
        (("ppr", -1, -1), ("ego", -1, 4), "EGO L=4 vs PPR @ unbudgeted", "EGO_L4_vs_PPR", "holm"),
        # Exploratory (BH-FDR): EGO vs bm25 at very different token budgets.
        (("bm25", 128000, -1), ("ego", -1, 4), "EGO L=4 ub vs bm25 128k", "EGO_vs_BM25_LARGE", "fdr"),
        (("bm25", 128000, -1), ("ego", 8000, 4), "EGO L=4 b=8k vs bm25 128k", "EGO_vs_BM25_LARGE", "fdr"),
        # Exploratory (BH-FDR): internal depth sweep, post-hoc.
        (("ppr", 8000, -1), ("ego", 8000, 2), "EGO L=2 vs PPR @ b=8000", "EGO_INTERNAL_DEPTH", "fdr"),
        (("ego", 8000, 0), ("ego", 8000, 2), "L=2 vs L=0 @ b=8000", "EGO_INTERNAL_DEPTH", "fdr"),
        (("ego", 8000, 2), ("ego", 8000, 4), "L=4 vs L=2 @ b=8000", "EGO_INTERNAL_DEPTH", "fdr"),
        (("ego", -1, 2), ("ego", -1, 4), "L=4 vs L=2 @ unbudgeted", "EGO_INTERNAL_DEPTH", "fdr"),
    ]


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--cells-dir", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--verbose", action="store_true")
    args = p.parse_args()

    args.out.mkdir(parents=True, exist_ok=True)

    sys.stderr.write(f"Loading checkpoints from {args.cells_dir}...\n")
    rows = load_long(args.cells_dir)
    sys.stderr.write(f"Loaded {len(rows)} per-instance rows\n")
    by_cell = long_to_arrays(rows)
    sys.stderr.write(f"Pivoted to {len(by_cell)} (method, budget, depth, dataset) cells\n")

    sys.stderr.write("Computing per-bucket recalls...\n")
    bucket_ratio = per_bucket_recall(by_cell, "gold_to_changed_ratio", _bucket_by_ratio)
    bucket_gold = per_bucket_recall(by_cell, "n_gold", _bucket_by_gold)
    sys.stderr.write(f"  ratio: {len(bucket_ratio)} rows; gold: {len(bucket_gold)} rows\n")

    sys.stderr.write("Running pairwise comparisons (paired bootstrap + Wilcoxon, Holm-corrected)...\n")
    pairs = _key_pairs()
    datasets = sorted({r["dataset"] for r in rows})
    pair_ratio = pairwise_comparisons(by_cell, pairs, datasets, "gold_to_changed_ratio", _bucket_by_ratio)
    pair_gold = pairwise_comparisons(by_cell, pairs, datasets, "n_gold", _bucket_by_gold)
    sys.stderr.write(f"  ratio: {len(pair_ratio)} comparisons; gold: {len(pair_gold)} comparisons\n")

    sys.stderr.write("Fitting continuous regression per method...\n")
    regs = regression_per_method(by_cell)
    sys.stderr.write(f"  {len(regs)} regression rows\n")

    sys.stderr.write("Per-language hard-regime breakdown...\n")
    lang_hard = per_language_hard_regime(by_cell)
    sys.stderr.write(f"  {len(lang_hard)} (cell, language) rows\n")

    sys.stderr.write("Matched-cardinality scan...\n")
    cardinality = matched_cardinality_scan(by_cell)

    # Write structured JSON for downstream consumers.
    payload = {
        "n_rows": len(rows),
        "n_cells": len(by_cell),
        "datasets": datasets,
        "ratio_buckets": [b[0] for b in _RATIO_BUCKETS],
        "gold_buckets": [b[0] for b in _GOLD_BUCKETS],
        "per_bucket_ratio": bucket_ratio,
        "per_bucket_gold": bucket_gold,
        "pairwise_ratio": pair_ratio,
        "pairwise_gold": pair_gold,
        "regression": regs,
        "per_language_hard": lang_hard,
        "matched_cardinality": cardinality,
    }
    (args.out / "stratified.json").write_text(json.dumps(payload, indent=2, default=str))

    # Markdown report
    bucket_labels_ratio = [b[0] for b in _RATIO_BUCKETS]
    bucket_labels_gold = [b[0] for b in _GOLD_BUCKETS]
    md_parts: list[str] = ["# Stratified analysis\n"]
    md_parts.append(_render_structural_fact(rows, datasets))
    md_parts.append(f"**Rows:** {len(rows)} per-instance records across {len(by_cell)} (method, budget, depth, dataset) cells.\n")
    md_parts.append(render_pooled_per_bucket_table(bucket_ratio, bucket_labels_ratio, "Recall by difficulty ratio"))
    md_parts.append(
        render_per_bucket_table(bucket_ratio, bucket_labels_ratio, "Recall by difficulty ratio (per-dataset, with CI)")
    )
    md_parts.append(render_pooled_per_bucket_table(bucket_gold, bucket_labels_gold, "Recall by |gold| (file count)"))
    md_parts.append(render_per_bucket_table(bucket_gold, bucket_labels_gold, "Recall by |gold| (per-dataset, with CI)"))
    md_parts.append(render_pairwise_table(pair_ratio, "Pairwise comparisons by difficulty ratio"))
    md_parts.append(render_pairwise_table(pair_gold, "Pairwise comparisons by |gold| bucket"))
    md_parts.append(render_regression_table(regs))
    md_parts.append(render_per_language_hard(lang_hard))
    md_parts.append(render_matched_cardinality(cardinality))

    (args.out / "STRATIFIED_REPORT.md").write_text("\n".join(md_parts))

    sys.stderr.write(f"Wrote: {args.out / 'STRATIFIED_REPORT.md'}\n")
    sys.stderr.write(f"Wrote: {args.out / 'stratified.json'}\n")

    if args.verbose:
        for r in regs:
            sys.stderr.write(
                f"  {r['method']} b={r['budget']} L={r['depth']} ratio_slope={r['ratio_slope']:+.3f} "
                f"CI=[{r['ratio_slope_ci'][0]:+.3f},{r['ratio_slope_ci'][1]:+.3f}]\n"
            )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
