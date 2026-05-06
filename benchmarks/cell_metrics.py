"""Per-cell metric aggregation from a checkpoint.jsonl file.

Produces the `cell_summary.json` consumed by `aggregate_sweep.py` and by
the bench-sweep workflow. Centralizing here lets us add F-beta, latency
percentiles, per-language breakdowns, robustness bins, and conditional
line-F1 once and have every sweep run pick them up.

CLI:
    python -m benchmarks.cell_metrics --ckpt path/to/X.checkpoint.jsonl

Writes a single JSON object to stdout. Stable, additive schema.
"""

from __future__ import annotations

import argparse
import json
import math
import statistics
import sys
from collections.abc import Iterable, Sequence
from pathlib import Path

_F_BETAS: tuple[float, ...] = (0.5, 1.0, 2.0)

_GOLD_BUCKETS: tuple[tuple[str, int, int | None], ...] = (
    ("1", 1, 1),
    ("2-3", 2, 3),
    ("4-7", 4, 7),
    ("8-15", 8, 15),
    ("16+", 16, None),
)
_RATIO_BUCKETS: tuple[tuple[str, float, float | None], ...] = (
    ("≤1.0", 0.0, 1.0),
    ("1.0-1.5", 1.0, 1.5),
    ("1.5-2.0", 1.5, 2.0),
    ("2.0-3.0", 2.0, 3.0),
    ("3.0+", 3.0, None),
)
_LATENCY_BREAKDOWN_FIELDS: tuple[str, ...] = (
    "parse_changed_ms",
    "universe_walk_ms",
    "discovery_ms",
    "parse_discovered_ms",
    "tokenization_ms",
    "scoring_selection_ms",
    "scoring_ms",
    "selection_ms",
    "candidate_count",
    "edge_count",
    "edges_before_cap",
    "edges_dropped_by_cap",
    "nodes_capped",
    "ppr_forward_pushes",
    "ppr_backward_pushes",
    "greedy_iters",
)
_PATCH_FIELDS: tuple[str, ...] = (
    "n_changed_files",
    "n_hunks",
    "diff_size_lines",
    "n_gold_lines",
)


def _safe_div(num: float, denom: float) -> float:
    return num / denom if denom > 0 else 0.0


def _percentile(sorted_values: Sequence[float], q: float) -> float:
    if not sorted_values:
        return 0.0
    if q <= 0:
        return float(sorted_values[0])
    if q >= 1:
        return float(sorted_values[-1])
    pos = q * (len(sorted_values) - 1)
    lo = math.floor(pos)
    hi = math.ceil(pos)
    if lo == hi:
        return float(sorted_values[lo])
    frac = pos - lo
    return float(sorted_values[lo] * (1 - frac) + sorted_values[hi] * frac)


def _percentile_block(values: Iterable[float]) -> dict[str, object]:
    vals = sorted(float(v) for v in values)
    if not vals:
        return {"mean": 0.0, "median": 0.0, "p25": 0.0, "p75": 0.0, "p95": 0.0, "p99": 0.0, "max": 0.0, "std": 0.0}
    mean = statistics.fmean(vals)
    return {
        "mean": mean,
        "median": _percentile(vals, 0.5),
        "p25": _percentile(vals, 0.25),
        "p75": _percentile(vals, 0.75),
        "p95": _percentile(vals, 0.95),
        "p99": _percentile(vals, 0.99),
        "max": vals[-1],
        "std": statistics.pstdev(vals) if len(vals) > 1 else 0.0,
    }


def _f_beta(p: float, r: float, beta: float) -> float:
    """Fβ = (1+β²) · P · R / (β² · P + R). Returns 0 when denom == 0."""
    if p <= 0 and r <= 0:
        return 0.0
    b2 = beta * beta
    denom = b2 * p + r
    return _safe_div((1 + b2) * p * r, denom)


def _f_beta_block(precisions: Sequence[float], recalls: Sequence[float]) -> dict[str, dict[str, float]]:
    out: dict[str, dict[str, float]] = {}
    for beta in _F_BETAS:
        scores = [_f_beta(p, r, beta) for p, r in zip(precisions, recalls)]
        if not scores:
            out[f"f{beta:g}"] = {"mean": 0.0}
            continue
        out[f"f{beta:g}"] = {"mean": statistics.fmean(scores), "median": _percentile(sorted(scores), 0.5)}
    return out


def _recall_histogram(values: Sequence[float]) -> dict[str, float]:
    if not values:
        return {"perfect_pct": 0.0, "zero_pct": 0.0, "partial_pct": 0.0}
    n = len(values)
    perfect = sum(1 for v in values if v >= 1.0 - 1e-9)
    zero = sum(1 for v in values if v <= 1e-9)
    return {
        "perfect_pct": 100.0 * perfect / n,
        "zero_pct": 100.0 * zero / n,
        "partial_pct": 100.0 * (n - perfect - zero) / n,
    }


def _by_language(rows: Sequence[dict]) -> dict[str, dict[str, float]]:
    groups: dict[str, list[dict]] = {}
    for r in rows:
        lang = str((r.get("extra") or {}).get("language", "unknown"))
        groups.setdefault(lang, []).append(r)
    out: dict[str, dict[str, float]] = {}
    for lang, rs in groups.items():
        recalls = [float(r.get("file_recall") or 0.0) for r in rs]
        precs = [float(r.get("file_precision") or 0.0) for r in rs]
        n = len(rs)
        if n == 0:
            continue
        mean_r = statistics.fmean(recalls)
        mean_p = statistics.fmean(precs)
        out[lang] = {
            "n": float(n),
            "file_recall": mean_r,
            "file_precision": mean_p,
            "f1": _f_beta(mean_p, mean_r, 1.0),
            "f2": _f_beta(mean_p, mean_r, 2.0),
        }
    return out


def _conditional_line_f1(rows: Sequence[dict]) -> dict[str, float] | None:
    """Mean line_f1 conditional on file_recall > 0 (the file was at least partly retrieved)."""
    cond = [float(r["line_f1"]) for r in rows if r.get("line_f1") is not None and float(r.get("file_recall") or 0.0) > 0.0]
    if not cond:
        return None
    return {"mean": statistics.fmean(cond), "n": float(len(cond))}


def _stratified_recall(rows: Sequence[dict], key_extractor) -> dict[str, dict[str, float]] | None:
    """Generic stratification: group rows by a numeric key, compute mean recall per bucket."""
    out: dict[str, dict[str, float]] = {}
    have_any = False
    for label, lo, hi in _GOLD_BUCKETS:
        bucket_rows = []
        for r in rows:
            val = key_extractor(r)
            if val is None:
                continue
            if val < lo:
                continue
            if hi is not None and val > hi:
                continue
            bucket_rows.append(r)
        if not bucket_rows:
            continue
        have_any = True
        recalls = [float(r.get("file_recall") or 0.0) for r in bucket_rows]
        precs = [float(r.get("file_precision") or 0.0) for r in bucket_rows]
        out[label] = {
            "n": float(len(bucket_rows)),
            "file_recall": statistics.fmean(recalls),
            "file_precision": statistics.fmean(precs),
            "f1": _f_beta(statistics.fmean(precs), statistics.fmean(recalls), 1.0),
            "f2": _f_beta(statistics.fmean(precs), statistics.fmean(recalls), 2.0),
        }
    return out if have_any else None


def _stratified_recall_by_ratio(rows: Sequence[dict]) -> dict[str, dict[str, float]] | None:
    out: dict[str, dict[str, float]] = {}
    have_any = False
    for label, lo, hi in _RATIO_BUCKETS:
        bucket_rows = []
        for r in rows:
            val = (r.get("extra") or {}).get("gold_to_changed_ratio")
            if val is None:
                continue
            v = float(val)
            if v < lo:
                continue
            if hi is not None and v > hi:
                continue
            bucket_rows.append(r)
        if not bucket_rows:
            continue
        have_any = True
        recalls = [float(r.get("file_recall") or 0.0) for r in bucket_rows]
        precs = [float(r.get("file_precision") or 0.0) for r in bucket_rows]
        out[label] = {
            "n": float(len(bucket_rows)),
            "file_recall": statistics.fmean(recalls),
            "file_precision": statistics.fmean(precs),
            "f1": _f_beta(statistics.fmean(precs), statistics.fmean(recalls), 1.0),
        }
    return out if have_any else None


def _gold_characterization(rows: Sequence[dict]) -> dict[str, float]:
    """Aggregate gold-side descriptors for the test set: % single-file, % multi-file, % whole-file.

    Returns empty dict if no row carries the gold flags (old checkpoints predating
    the evaluator stamp). Callers should drop the section when empty.
    """
    n = len(rows)
    if n == 0:
        return {}
    have_flags = any("is_single_file_gold" in (r.get("extra") or {}) for r in rows)
    if not have_flags:
        return {}
    n_single = sum(1 for r in rows if (r.get("extra") or {}).get("is_single_file_gold"))
    n_multi = sum(1 for r in rows if (r.get("extra") or {}).get("is_multi_file_gold"))
    n_whole = sum(1 for r in rows if (r.get("extra") or {}).get("is_whole_file_gold"))
    n_zero = sum(1 for r in rows if (float((r.get("extra") or {}).get("n_gold") or 0)) == 0)
    return {
        "single_file_pct": 100.0 * n_single / n,
        "multi_file_pct": 100.0 * n_multi / n,
        "whole_file_pct": 100.0 * n_whole / n,
        "zero_gold_pct": 100.0 * n_zero / n,
    }


def _latency_breakdown(rows: Sequence[dict]) -> dict[str, dict[str, object]] | None:
    """Aggregate diffctx LatencyBreakdown sub-fields. Empty dict when nothing emitted."""
    out: dict[str, dict[str, object]] = {}
    for field in _LATENCY_BREAKDOWN_FIELDS:
        vals: list[float] = []
        for r in rows:
            ex = r.get("extra") or {}
            lb = ex.get("latency_breakdown") or {}
            if field in lb:
                vals.append(float(lb[field]))
        if vals:
            out[field] = _percentile_block(vals)
    return out if out else None


def _patch_size_distributions(rows: Sequence[dict]) -> dict[str, dict[str, object]]:
    """Aggregate patch-size descriptors stamped by the evaluator into extra."""
    out: dict[str, dict[str, object]] = {}
    for field in _PATCH_FIELDS:
        vals = [float((r.get("extra") or {}).get(field, 0)) for r in rows if (r.get("extra") or {}).get(field) is not None]
        if vals:
            out[field] = _percentile_block(vals)
    return out


def _collect_cardinality(rows: Sequence[dict]) -> dict[str, list[float]]:
    keys = ("n_selected", "n_gold", "fragment_count", "selected_to_gold_ratio", "gold_to_changed_ratio")
    out: dict[str, list[float]] = {k: [] for k in keys}
    for r in rows:
        ex = r.get("extra") or {}
        for k in keys:
            if k in ex:
                out[k].append(float(ex[k]))
    return out


def _collect_status_breakdown(rows: Sequence[dict]) -> tuple[dict[str, int], dict[str, int]]:
    statuses: dict[str, int] = {}
    errors: dict[str, int] = {}
    for r in rows:
        s = str((r.get("extra") or {}).get("status", "missing"))
        statuses[s] = statuses.get(s, 0) + 1
        if s != "ok":
            err = str((r.get("extra") or {}).get("error", "")).strip()
            if err:
                errors[err] = errors.get(err, 0) + 1
    return statuses, errors


def compute_cell_summary(rows: Sequence[dict]) -> dict:
    n = len(rows)
    if n == 0:
        return {"n": 0}

    recall = [float(r.get("file_recall") or 0.0) for r in rows]
    precision = [float(r.get("file_precision") or 0.0) for r in rows]
    elapsed = [float(r.get("elapsed_seconds") or 0.0) for r in rows]
    tokens = [float(r.get("used_tokens") or 0.0) for r in rows]

    frag_recall = [float(r["fragment_recall"]) for r in rows if r.get("fragment_recall") is not None]
    frag_precision = [float(r["fragment_precision"]) for r in rows if r.get("fragment_precision") is not None]
    line_f1 = [float(r["line_f1"]) for r in rows if r.get("line_f1") is not None]

    cardinality = _collect_cardinality(rows)
    statuses, errors = _collect_status_breakdown(rows)
    ok = statuses.get("ok", 0)

    rec_block = _percentile_block(recall)
    rec_block["hist"] = _recall_histogram(recall)
    prec_block = _percentile_block(precision)

    out: dict = {
        "n": n,
        "ok": ok,
        "ok_pct": 100.0 * ok / n,
        "statuses": statuses,
        "errors": dict(sorted(errors.items(), key=lambda x: -x[1])[:10]),
        "file_recall": rec_block,
        "file_precision": prec_block,
        "file_fbeta": _f_beta_block(precision, recall),
        "fragment_recall": ({"mean": statistics.fmean(frag_recall), "n_with_gold": len(frag_recall)} if frag_recall else None),
        "fragment_precision": (
            {"mean": statistics.fmean(frag_precision), "n_with_gold": len(frag_precision)} if frag_precision else None
        ),
        "fragment_fbeta": (_f_beta_block(frag_precision, frag_recall) if frag_recall and frag_precision else None),
        "line_f1": (
            {
                "mean": statistics.fmean(line_f1),
                "n_with_gold": len(line_f1),
                "conditional_on_file_hit": _conditional_line_f1(rows),
            }
            if line_f1
            else None
        ),
        "elapsed_seconds": _percentile_block(elapsed),
        "used_tokens": _percentile_block(tokens),
        "by_language": _by_language(rows),
    }
    for key, values in cardinality.items():
        if values:
            out[key] = _percentile_block(values)

    extras: list[tuple[str, object]] = [
        ("patch_size", _patch_size_distributions(rows)),
        ("gold_characterization", _gold_characterization(rows)),
        ("recall_by_gold_size", _stratified_recall(rows, lambda r: float((r.get("extra") or {}).get("n_gold") or 0))),
        ("recall_by_difficulty_ratio", _stratified_recall_by_ratio(rows)),
        ("latency_breakdown", _latency_breakdown(rows)),
    ]
    for key, value in extras:
        if value:
            out[key] = value
    return out


def load_jsonl(path: Path) -> list[dict]:
    rows: list[dict] = []
    with path.open() as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except ValueError:
                continue
    return rows


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--ckpt", type=Path, required=True, help="path to <test_set>.checkpoint.jsonl")
    p.add_argument("--out", type=Path, default=None, help="output path (default: stdout)")
    args = p.parse_args()

    if not args.ckpt.exists():
        sys.stderr.write(f"checkpoint not found: {args.ckpt}\n")
        payload = {"error": "no checkpoint produced", "expected_path": str(args.ckpt)}
    else:
        rows = load_jsonl(args.ckpt)
        payload = compute_cell_summary(rows)
        if payload.get("errors"):
            sys.stderr.write("=== ERROR BREAKDOWN ===\n")
            for msg, cnt in sorted(payload["errors"].items(), key=lambda x: -x[1]):
                sys.stderr.write(f"  [{cnt}x] {msg[:200]}\n")

    text = json.dumps(payload, indent=2, default=str)
    if args.out:
        args.out.write_text(text)
    else:
        sys.stdout.write(text + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
