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
import sys
from collections import defaultdict
from collections.abc import Iterable, Sequence
from pathlib import Path

import numpy as np

from benchmarks.stats import bootstrap_ci, holm_correct, paired_bootstrap_delta, wilcoxon_paired

_RATIO_BUCKETS: tuple[tuple[str, float, float | None], ...] = (
    ("≤1.0", 0.0, 1.0),
    ("1.0-1.5", 1.0 + 1e-9, 1.5),
    ("1.5-2.0", 1.5 + 1e-9, 2.0),
    ("2.0-3.0", 2.0 + 1e-9, 3.0),
    ("3.0+", 3.0 + 1e-9, None),
)
_GOLD_BUCKETS: tuple[tuple[str, int, int | None], ...] = (
    ("1", 1, 1),
    ("2-3", 2, 3),
    ("4-7", 4, 7),
    ("8-15", 8, 15),
    ("16+", 16, None),
)


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
            mask &= values <= hi
        out[label] = mask
    return out


def _bucket_by_gold(values: np.ndarray) -> dict[str, np.ndarray]:
    out: dict[str, np.ndarray] = {}
    for label, lo, hi in _GOLD_BUCKETS:
        mask = values >= lo
        if hi is not None:
            mask &= values <= hi
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
) -> tuple[list[float], list[float]]:
    ids_a = {iid: idx for idx, iid in enumerate(cell_a["instance_id"])}
    masks = bucket_fn(cell_a[bucket_field])
    mask_a = masks[bucket_label]
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
    return a_vals, b_vals


def pairwise_comparisons(
    by_cell: dict[tuple[str, int, int, str], dict[str, np.ndarray]],
    pairs: Sequence[tuple[tuple[str, int, int], tuple[str, int, int], str]],
    datasets: Iterable[str],
    bucket_field: str,
    bucket_fn,
) -> list[dict]:
    """Paired bootstrap + Wilcoxon for each (pair, dataset, bucket)."""
    out: list[dict] = []
    for a_cfg, b_cfg, label in pairs:
        for ds in datasets:
            cell_a = by_cell.get((*a_cfg, ds))
            cell_b = by_cell.get((*b_cfg, ds))
            if cell_a is None or cell_b is None:
                continue
            bucket_defs = _RATIO_BUCKETS if bucket_field == "gold_to_changed_ratio" else _GOLD_BUCKETS
            for bucket_def in bucket_defs:
                bucket_label = bucket_def[0]
                a_vals, b_vals = _paired_pull(cell_a, cell_b, bucket_label, bucket_field, bucket_fn)
                if len(a_vals) < 5:
                    continue
                boot = paired_bootstrap_delta(a_vals, b_vals, n_iter=5000)
                wilc = wilcoxon_paired(a_vals, b_vals)
                out.append(
                    {
                        "pair_label": label,
                        "a": f"{a_cfg[0]} b={a_cfg[1]} L={a_cfg[2]}",
                        "b": f"{b_cfg[0]} b={b_cfg[1]} L={b_cfg[2]}",
                        "dataset": ds,
                        "bucket": bucket_label,
                        "n": len(a_vals),
                        "mean_a": float(np.mean(a_vals)),
                        "mean_b": float(np.mean(b_vals)),
                        "delta": boot["delta_mean"],
                        "ci_lo": boot["ci_lo"],
                        "ci_hi": boot["ci_hi"],
                        "p_boot": boot["p_value"],
                        "p_wilcoxon": wilc["p_value"],
                    }
                )
    # Holm correction across the family of bootstrap p-values
    if out:
        ps = [r["p_boot"] for r in out]
        corrected = holm_correct(ps)
        for r, c in zip(out, corrected):
            r["p_holm"] = c["p_adj"]
            r["holm_reject"] = c["rejected"]
    return out


# ---------------------------------------------------------------------------
# Continuous regression
# ---------------------------------------------------------------------------


def regression_per_method(
    by_cell: dict[tuple[str, int, int, str], dict[str, np.ndarray]],
) -> list[dict]:
    """Fit `recall = a + b * log1p(ratio) + c * log1p(n_gold)` per (method, budget, depth)."""
    by_cfg: dict[tuple[str, int, int], list[dict[str, np.ndarray]]] = defaultdict(list)
    for cell_key, arrs in by_cell.items():
        method, budget, depth, _dataset = cell_key
        if budget == 0:
            continue
        by_cfg[(method, budget, depth)].append(arrs)

    out: list[dict] = []
    for cfg, arr_list in by_cfg.items():
        recall = np.concatenate([a["file_recall"] for a in arr_list]).astype(float)
        ratio = np.concatenate([a["gold_to_changed_ratio"] for a in arr_list]).astype(float)
        n_gold = np.concatenate([a["n_gold"] for a in arr_list]).astype(float)
        if len(recall) < 30:
            continue
        x_ratio = np.log1p(ratio)
        x_gold = np.log1p(n_gold)
        x = np.column_stack([np.ones_like(recall), x_ratio, x_gold])
        try:
            coef = np.linalg.lstsq(x, recall, rcond=None)[0]
        except np.linalg.LinAlgError:
            continue
        # Bootstrap CI on each coefficient
        rng = np.random.default_rng(42)
        boot_coefs = np.zeros((1000, 3), dtype=np.float64)
        n = len(recall)
        for i in range(1000):
            idx = rng.integers(0, n, n)
            try:
                bc = np.linalg.lstsq(x[idx], recall[idx], rcond=None)[0]
                boot_coefs[i] = bc
            except np.linalg.LinAlgError:
                boot_coefs[i] = coef
        ci_lo = np.percentile(boot_coefs, 2.5, axis=0)
        ci_hi = np.percentile(boot_coefs, 97.5, axis=0)
        out.append(
            {
                "method": cfg[0],
                "budget": cfg[1],
                "depth": cfg[2],
                "n": int(n),
                "intercept": float(coef[0]),
                "intercept_ci": (float(ci_lo[0]), float(ci_hi[0])),
                "ratio_slope": float(coef[1]),
                "ratio_slope_ci": (float(ci_lo[1]), float(ci_hi[1])),
                "gold_slope": float(coef[2]),
                "gold_slope_ci": (float(ci_lo[2]), float(ci_hi[2])),
            }
        )
    return out


# ---------------------------------------------------------------------------
# Per-language hard regime (ratio>1.5)
# ---------------------------------------------------------------------------


def per_language_hard_regime(by_cell: dict[tuple[str, int, int, str], dict[str, np.ndarray]]) -> list[dict]:
    out: list[dict] = []
    for (method, budget, depth, dataset), arrs in by_cell.items():
        if budget == 0:
            continue
        mask = arrs["gold_to_changed_ratio"] > 1.5
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
    return f"{rec['mean_recall']:.3f} [{rec['ci_lo']:.3f}, {rec['ci_hi']:.3f}] (n={rec['n']})"


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
    """Pool over datasets, weighted by n. Shows one row per (method, budget, depth)."""
    pooled: dict[tuple[str, int, int], dict[str, list[dict]]] = defaultdict(lambda: defaultdict(list))
    for r in rows:
        pooled[(r["method"], r["budget"], r["depth"])][r["bucket"]].append(r)

    out: list[str] = [f"\n## {title} (pooled across datasets)", ""]
    out.append("Pooled means weighted by per-dataset n. CI is half-width approximation `sqrt(Σnᵢ·varᵢ)/Σnᵢ`.")
    out.append("")
    out.append("| method | budget | depth | " + " | ".join(buckets) + " |")
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


def render_pairwise_table(rows: list[dict], title: str) -> str:
    if not rows:
        return ""
    out: list[str] = [f"\n## {title}", ""]
    out.append(
        "Δ is paired (instance-id matched). 95% bootstrap CI from 5000 resamples on the per-instance Δ. Holm-corrected p adjusts across the entire pair x dataset x bucket family below."
    )
    out.append("")
    out.append("| pair | dataset | bucket | n | mean_a | mean_b | Δ (b-a) | 95% CI | Wilcoxon p | Holm p | reject H₀? |")
    out.append("|---|---|---|---:|---:|---:|---:|---|---:|---:|---|")
    for r in rows:
        ci = f"[{r['ci_lo']:+.3f}, {r['ci_hi']:+.3f}]"
        reject = "✓" if r.get("holm_reject") else "—"
        out.append(
            f"| {r['pair_label']} | {r['dataset']} | {r['bucket']} | {r['n']} | "
            f"{r['mean_a']:.3f} | {r['mean_b']:.3f} | {r['delta']:+.3f} | {ci} | "
            f"{r['p_wilcoxon']:.3g} | {r['p_holm']:.3g} | {reject} |"
        )
    return "\n".join(out) + "\n"


def render_regression_table(rows: list[dict]) -> str:
    if not rows:
        return ""
    out: list[str] = ["\n## Continuous regression: recall ~ log(1+ratio) + log(1+|gold|)", ""]
    out.append(
        "Larger `ratio_slope` (less negative) = better scaling on hard instances. Larger `gold_slope` (less negative) = better scaling on multi-file gold."
    )
    out.append("")
    out.append("| method | budget | depth | n | intercept | ratio_slope | ratio CI | gold_slope | gold CI |")
    out.append("|---|---:|---:|---:|---:|---:|---|---:|---|")
    method_order = ["aider", "bm25", "ppr", "ego"]
    rows_sorted = sorted(
        rows, key=lambda r: (method_order.index(r["method"]) if r["method"] in method_order else 99, r["budget"], r["depth"])
    )
    for r in rows_sorted:
        ratio_ci = f"[{r['ratio_slope_ci'][0]:+.3f}, {r['ratio_slope_ci'][1]:+.3f}]"
        gold_ci = f"[{r['gold_slope_ci'][0]:+.3f}, {r['gold_slope_ci'][1]:+.3f}]"
        out.append(
            f"| **{r['method']}** | {r['budget']} | {r['depth']} | {r['n']} | "
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


def _key_pairs() -> list[tuple[tuple[str, int, int], tuple[str, int, int], str]]:
    """Headline method comparisons.

    `(a_cfg, b_cfg, label)`. Δ in the table is `b - a` (so positive = b wins).
    """
    return [
        (("ppr", 8000, -1), ("ego", 8000, 4), "EGO L=4 vs PPR @ b=8000"),
        (("ppr", 8000, -1), ("ego", 8000, 2), "EGO L=2 vs PPR @ b=8000"),
        (("ego", 8000, 0), ("ego", 8000, 4), "EGO L=4 vs L=0 @ b=8000"),
        (("ego", 8000, 0), ("ego", 8000, 2), "EGO L=2 vs L=0 @ b=8000"),
        (("ego", 8000, 2), ("ego", 8000, 4), "EGO L=4 vs L=2 @ b=8000"),
        (("ppr", -1, -1), ("ego", -1, 4), "EGO L=4 vs PPR @ unbudgeted"),
        (("bm25", 128000, -1), ("ego", -1, 4), "EGO L=4 ub vs bm25 128k"),
        (("bm25", 128000, -1), ("ego", 8000, 4), "EGO L=4 b=8k vs bm25 128k"),
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
    md_parts.append(f"**Rows:** {len(rows)} per-instance records across {len(by_cell)} (method, budget, depth, dataset) cells.\n")
    md_parts.append(render_pooled_per_bucket_table(bucket_ratio, bucket_labels_ratio, "Recall by difficulty ratio"))
    md_parts.append(
        render_per_bucket_table(bucket_ratio, bucket_labels_ratio, "Recall by difficulty ratio (per-dataset, with CI)")
    )
    md_parts.append(render_pooled_per_bucket_table(bucket_gold, bucket_labels_gold, "Recall by |gold| (file count)"))
    md_parts.append(render_per_bucket_table(bucket_gold, bucket_labels_gold, "Recall by |gold| (per-dataset, with CI)"))
    md_parts.append(render_pairwise_table(pair_ratio, "Pairwise comparisons by difficulty ratio (Holm-corrected)"))
    md_parts.append(render_pairwise_table(pair_gold, "Pairwise comparisons by |gold| bucket (Holm-corrected)"))
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
