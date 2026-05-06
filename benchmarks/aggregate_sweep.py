"""Aggregate per-cell sweep artifacts into one summary JSON + markdown table.

Input layout (from `actions/download-artifact@v4` with pattern=cell-*):
    <cells-dir>/cell-<method>-b<budget>-L<depth>-<test_set>/
        metadata.json
        cell_summary.json
        <test_set>.checkpoint.jsonl
        <test_set>.json
        run.log
        system_info.log

Legacy layout `cell-<method>-b<budget>-<test_set>/` is still parsed (depth=-1
sentinel meaning "method does not consume depth") so old artifact dumps
remain readable.

Output:
    <out>/grand_summary.json   — every cell's metadata + summary in one file
    <out>/SWEEP_TABLE.md       — markdown matrix of mean recall per cell
    <out>/cell_index.csv       — flat row-per-cell CSV for further analysis

The aggregator is permissive: missing artifacts are reported but do not
cause the script to fail (so partial sweeps still produce useful output).
"""

from __future__ import annotations

import argparse
import csv
import json
from collections import defaultdict
from pathlib import Path


def _load_jsonl(path: Path) -> list[dict]:
    if not path.exists():
        return []
    out: list[dict] = []
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            out.append(json.loads(line))
        except ValueError:
            continue
    return out


def _safe_load(path: Path) -> dict | None:
    if not path.exists():
        return None
    try:
        return json.loads(path.read_text())
    except (ValueError, OSError):
        return None


def collect_cells(cells_dir: Path) -> list[dict]:
    """Walk every cell-* artifact directory, return flat per-cell records."""
    cells: list[dict] = []
    for cell_root in sorted(cells_dir.iterdir()):
        if not cell_root.is_dir() or not cell_root.name.startswith("cell-"):
            continue
        meta = _safe_load(cell_root / "metadata.json") or {}
        summary = _safe_load(cell_root / "cell_summary.json") or {}
        # Find the per-instance checkpoint
        ckpts = sorted(cell_root.glob("*.checkpoint.jsonl"))
        rows = _load_jsonl(ckpts[0]) if ckpts else []
        cell_info = meta.get("cell") or {}
        parsed = _parse_artifact(cell_root.name)
        cells.append(
            {
                "artifact_dir": cell_root.name,
                "method": cell_info.get("method") or parsed[0],
                "budget": cell_info.get("budget") if cell_info.get("budget") is not None else parsed[1],
                "depth": cell_info.get("depth") if cell_info.get("depth") is not None else parsed[2],
                "test_set": cell_info.get("test_set") or parsed[3],
                "metadata": meta,
                "summary": summary,
                "n_instances": len(rows),
                "instance_recall_values": [r.get("file_recall", 0.0) for r in rows],
            }
        )
    return cells


# New artifact layout: cell-<method>-b<budget>-L<depth>-<test_set>
_ARTIFACT_RE_WITH_DEPTH = __import__("re").compile(
    r"^cell-(?P<method>[a-zA-Z0-9_]+)-b(?P<budget>-?\d+)-L(?P<depth>-?\d+)-(?P<test_set>.+)$"
)
# Legacy artifact layout: cell-<method>-b<budget>-<test_set> (no depth segment)
_ARTIFACT_RE_LEGACY = __import__("re").compile(r"^cell-(?P<method>[a-zA-Z0-9_]+)-b(?P<budget>-?\d+)-(?P<test_set>.+)$")


def _parse_artifact(name: str) -> tuple[str | None, int | None, int | None, str | None]:
    """Parse a `cell-<method>-b<budget>-L<depth>-<test_set>` directory name.

    Returns (method, budget, depth, test_set). For legacy artifacts that
    predate the depth axis, depth resolves to -1 (the sentinel meaning
    "method does not consume depth"). Used as a fallback when
    `metadata.json` was not produced (e.g., the cell crashed before the
    metadata step ran).
    """
    m = _ARTIFACT_RE_WITH_DEPTH.match(name)
    if m:
        try:
            budget = int(m.group("budget"))
        except ValueError:
            budget = None
        try:
            depth: int | None = int(m.group("depth"))
        except ValueError:
            depth = None
        return (m.group("method"), budget, depth, m.group("test_set"))
    m = _ARTIFACT_RE_LEGACY.match(name)
    if not m:
        return (None, None, None, None)
    try:
        budget = int(m.group("budget"))
    except ValueError:
        budget = None
    return (m.group("method"), budget, -1, m.group("test_set"))


_METHOD_ORDER = ["ppr", "ego", "bm25", "aider"]


def _method_sort_key(method: str) -> int:
    return _METHOD_ORDER.index(method) if method in _METHOD_ORDER else 99


def _format_sweep_cell(cell: dict | None) -> str:
    if not cell:
        return "| --"
    summary = cell["summary"]
    fr = (summary.get("file_recall") or {}).get("mean")
    n = summary.get("n", 0)
    ok = summary.get("ok", 0)
    return f"| n={n}" if fr is None else f"| {fr:.3f} (n={ok}/{n})"


def render_sweep_table(cells: list[dict]) -> str:
    by_set: dict[str, dict[tuple[str, int], dict]] = defaultdict(dict)
    methods: set[str] = set()
    budgets: set[int] = set()
    for c in cells:
        m, b, ts = c["method"], c["budget"], c["test_set"]
        if m is None or b is None or ts is None:
            continue
        methods.add(m)
        budgets.add(b)
        by_set[ts][(m, b)] = c

    methods_sorted = sorted(methods, key=_method_sort_key)
    budgets_sorted = sorted(budgets)

    lines: list[str] = ["# Sweep results — mean file recall (and ok-instance count)\n"]
    for ts in sorted(by_set):
        lines.append(f"## {ts}\n")
        header = "| method \\ budget | " + " | ".join(str(b) if b >= 0 else "-1 (∞)" for b in budgets_sorted) + " |"
        sep = "|" + " --- |" * (1 + len(budgets_sorted))
        lines.append(header)
        lines.append(sep)
        for m in methods_sorted:
            row = [f"| **{m}** "] + [_format_sweep_cell(by_set[ts].get((m, b))) for b in budgets_sorted] + ["|"]
            lines.append("".join(row))
        lines.append("")
    return "\n".join(lines) + "\n"


def _cell_metric(cell: dict, getter) -> float | None:
    s = cell.get("summary") or {}
    try:
        return getter(s)
    except (KeyError, TypeError, AttributeError):
        return None


def render_headline_tables(cells: list[dict]) -> str:
    """Multi-section headline. F1/F2 + per-language + robustness + tokens/latency p95.

    Each (method, budget, depth) line shows the mean across the three datasets the
    cell was evaluated against, mirroring the headline format the team uses.
    """
    if not cells:
        return ""

    valid = [c for c in cells if c["method"] and c["budget"] is not None and c["test_set"]]
    by_cfg: dict[tuple[str, int, int], list[dict]] = defaultdict(list)
    for c in valid:
        by_cfg[(c["method"], c["budget"], c.get("depth") or -1)].append(c)

    def mean_of(cells_for_cfg, getter) -> float | None:
        vals = [v for v in (_cell_metric(c, getter) for c in cells_for_cfg) if v is not None]
        return sum(vals) / len(vals) if vals else None

    def fmt(v: float | None, ndigits: int = 4) -> str:
        return f"{v:.{ndigits}f}" if v is not None else "—"

    sorted_cfgs = sorted(by_cfg.keys(), key=lambda k: (_method_sort_key(k[0]), int(k[1]), int(k[2])))

    out: list[str] = []
    out.append("\n## Headline by F-beta (mean across datasets)")
    out.append("")
    out.append("| method | budget | depth | recall | precision | F0.5 | F1 | F2 | tokens p50 | tokens p95 |")
    out.append("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
    for cfg in sorted_cfgs:
        cs = by_cfg[cfg]
        recall = mean_of(cs, lambda s: s["file_recall"]["mean"])
        prec = mean_of(cs, lambda s: s["file_precision"]["mean"])
        f1 = mean_of(cs, lambda s: s["file_fbeta"]["f1"]["mean"])
        f2 = mean_of(cs, lambda s: s["file_fbeta"]["f2"]["mean"])
        f05 = mean_of(cs, lambda s: s["file_fbeta"]["f0.5"]["mean"])
        tk_p50 = mean_of(cs, lambda s: s["used_tokens"]["median"])
        tk_p95 = mean_of(cs, lambda s: s["used_tokens"]["p95"])
        out.append(
            f"| **{cfg[0]}** | {cfg[1]} | {cfg[2]} | {fmt(recall)} | {fmt(prec)} | "
            f"{fmt(f05)} | {fmt(f1)} | {fmt(f2)} | "
            f"{fmt(tk_p50, 0)} | {fmt(tk_p95, 0)} |"
        )

    out.append("\n## Robustness — recall distribution (mean across datasets)")
    out.append("")
    out.append("| method | budget | depth | %perfect | %zero | %partial | recall std |")
    out.append("|---|---:|---:|---:|---:|---:|---:|")
    for cfg in sorted_cfgs:
        cs = by_cfg[cfg]
        perfect = mean_of(cs, lambda s: s["file_recall"]["hist"]["perfect_pct"])
        zero = mean_of(cs, lambda s: s["file_recall"]["hist"]["zero_pct"])
        partial = mean_of(cs, lambda s: s["file_recall"]["hist"]["partial_pct"])
        std = mean_of(cs, lambda s: s["file_recall"]["std"])
        out.append(
            f"| **{cfg[0]}** | {cfg[1]} | {cfg[2]} | " f"{fmt(perfect, 1)} | {fmt(zero, 1)} | {fmt(partial, 1)} | {fmt(std, 3)} |"
        )

    out.append("\n## Latency — elapsed_seconds across datasets")
    out.append("")
    out.append("| method | budget | depth | mean | p50 | p95 | p99 |")
    out.append("|---|---:|---:|---:|---:|---:|---:|")
    for cfg in sorted_cfgs:
        cs = by_cfg[cfg]
        mean = mean_of(cs, lambda s: s["elapsed_seconds"]["mean"])
        p50 = mean_of(cs, lambda s: s["elapsed_seconds"]["median"])
        p95 = mean_of(cs, lambda s: s["elapsed_seconds"]["p95"])
        p99 = mean_of(cs, lambda s: s["elapsed_seconds"]["p99"])
        out.append(f"| **{cfg[0]}** | {cfg[1]} | {cfg[2]} | {fmt(mean, 2)} | {fmt(p50, 2)} | {fmt(p95, 2)} | {fmt(p99, 2)} |")

    cardinality_present = any((c.get("summary") or {}).get("n_selected") for c in valid)
    fragment_present = any((c.get("summary") or {}).get("fragment_count") for c in valid)
    if cardinality_present or fragment_present:
        out.append("\n## Selection cardinality (files / fragments)")
        out.append("")
        if cardinality_present:
            out.append("| method | budget | depth | n_selected p50 | n_selected p95 | n_gold p50 |")
            out.append("|---|---:|---:|---:|---:|---:|")
            for cfg in sorted_cfgs:
                cs = by_cfg[cfg]
                n_sel_p50 = mean_of(cs, lambda s: s["n_selected"]["median"]) if cardinality_present else None
                n_sel_p95 = mean_of(cs, lambda s: s["n_selected"]["p95"]) if cardinality_present else None
                n_gold_p50 = mean_of(cs, lambda s: s["n_gold"]["median"]) if cardinality_present else None
                out.append(
                    f"| **{cfg[0]}** | {cfg[1]} | {cfg[2]} | "
                    f"{fmt(n_sel_p50, 1)} | {fmt(n_sel_p95, 1)} | {fmt(n_gold_p50, 1)} |"
                )
        else:
            out.append("| method | budget | depth | fragment_count p50 | p95 |")
            out.append("|---|---:|---:|---:|---:|")
            for cfg in sorted_cfgs:
                cs = by_cfg[cfg]
                fc_p50 = mean_of(cs, lambda s: s["fragment_count"]["median"])
                fc_p95 = mean_of(cs, lambda s: s["fragment_count"]["p95"])
                out.append(f"| **{cfg[0]}** | {cfg[1]} | {cfg[2]} | {fmt(fc_p50, 1)} | {fmt(fc_p95, 1)} |")

    return "\n".join(out) + "\n"


def _aggregate_languages(cells: list[dict]) -> dict[tuple[str, int, int], dict[str, dict[str, float]]]:
    out: dict[tuple[str, int, int], dict[str, dict[str, float]]] = {}
    for c in cells:
        m, b, d = c["method"], c["budget"], c.get("depth") or -1
        if m is None or b is None:
            continue
        cfg = (m, b, d)
        per_lang = (c.get("summary") or {}).get("by_language") or {}
        if not per_lang:
            continue
        bucket = out.setdefault(cfg, {})
        for lang, agg in per_lang.items():
            cur = bucket.setdefault(lang, {"n": 0.0, "recall_sum": 0.0, "precision_sum": 0.0, "f1_sum": 0.0, "f2_sum": 0.0})
            n = float(agg.get("n", 0))
            cur["n"] += n
            cur["recall_sum"] += float(agg.get("file_recall", 0.0)) * n
            cur["precision_sum"] += float(agg.get("file_precision", 0.0)) * n
            cur["f1_sum"] += float(agg.get("f1", 0.0)) * n
            cur["f2_sum"] += float(agg.get("f2", 0.0)) * n
    finalized: dict[tuple[str, int, int], dict[str, dict[str, float]]] = {}
    for cfg, langs in out.items():
        finalized[cfg] = {
            lang: {
                "n": v["n"],
                "file_recall": v["recall_sum"] / v["n"] if v["n"] else 0.0,
                "file_precision": v["precision_sum"] / v["n"] if v["n"] else 0.0,
                "f1": v["f1_sum"] / v["n"] if v["n"] else 0.0,
                "f2": v["f2_sum"] / v["n"] if v["n"] else 0.0,
            }
            for lang, v in langs.items()
        }
    return finalized


def render_pipeline_tables(cells: list[dict]) -> str:
    """Latency breakdown + graph stats — only emitted when at least one cell has them."""
    if not cells:
        return ""
    valid = [c for c in cells if c["method"] and c["budget"] is not None and c["test_set"]]
    by_cfg: dict[tuple[str, int, int], list[dict]] = defaultdict(list)
    for c in valid:
        by_cfg[(c["method"], c["budget"], c.get("depth") or -1)].append(c)

    def has_field(field: str) -> bool:
        for c in valid:
            lb = (c.get("summary") or {}).get("latency_breakdown") or {}
            if field in lb:
                return True
        return False

    def mean_of(cells_for_cfg, getter) -> float | None:
        vals = [v for v in (_cell_metric(c, getter) for c in cells_for_cfg) if v is not None]
        return sum(vals) / len(vals) if vals else None

    def fmt(v: float | None, ndigits: int = 1) -> str:
        return f"{v:.{ndigits}f}" if v is not None else "—"

    cfgs = sorted(by_cfg.keys(), key=lambda k: (_method_sort_key(k[0]), int(k[1]), int(k[2])))

    out: list[str] = []

    if has_field("scoring_ms") or has_field("discovery_ms"):
        out.append("\n## Pipeline latency breakdown (median, ms)")
        out.append("")
        out.append("| method | budget | depth | parse | discover | tokenize | scoring | selection |")
        out.append("|---|---:|---:|---:|---:|---:|---:|---:|")
        for cfg in cfgs:
            cs = by_cfg[cfg]
            parse = mean_of(cs, lambda s: s["latency_breakdown"]["parse_changed_ms"]["median"])
            discov = mean_of(cs, lambda s: s["latency_breakdown"]["discovery_ms"]["median"])
            token = mean_of(cs, lambda s: s["latency_breakdown"]["tokenization_ms"]["median"])
            scoring = mean_of(cs, lambda s: s["latency_breakdown"]["scoring_ms"]["median"])
            selection = mean_of(cs, lambda s: s["latency_breakdown"]["selection_ms"]["median"])
            out.append(
                f"| **{cfg[0]}** | {cfg[1]} | {cfg[2]} | "
                f"{fmt(parse)} | {fmt(discov)} | {fmt(token)} | {fmt(scoring)} | {fmt(selection)} |"
            )

    if has_field("edge_count"):
        out.append("\n## Graph size — edges and pushes (median per instance)")
        out.append("")
        out.append("| method | budget | depth | candidates | edges | edges_dropped | nodes_capped | ppr_fwd | ppr_bwd |")
        out.append("|---|---:|---:|---:|---:|---:|---:|---:|---:|")
        for cfg in cfgs:
            cs = by_cfg[cfg]
            cand = mean_of(cs, lambda s: s["latency_breakdown"]["candidate_count"]["median"])
            edges = mean_of(cs, lambda s: s["latency_breakdown"]["edge_count"]["median"])
            dropped = mean_of(cs, lambda s: s["latency_breakdown"]["edges_dropped_by_cap"]["median"])
            nodes_capped = mean_of(cs, lambda s: s["latency_breakdown"]["nodes_capped"]["median"])
            ppr_fwd = mean_of(cs, lambda s: s["latency_breakdown"]["ppr_forward_pushes"]["median"])
            ppr_bwd = mean_of(cs, lambda s: s["latency_breakdown"]["ppr_backward_pushes"]["median"])
            out.append(
                f"| **{cfg[0]}** | {cfg[1]} | {cfg[2]} | "
                f"{fmt(cand, 0)} | {fmt(edges, 0)} | {fmt(dropped, 0)} | {fmt(nodes_capped, 0)} | "
                f"{fmt(ppr_fwd, 0)} | {fmt(ppr_bwd, 0)} |"
            )

    return "\n".join(out) + "\n" if out else ""


def _avg_recall_for_bucket(cells_for_cfg: list[dict], strat_key: str, bucket: str) -> float | None:
    vals: list[float] = []
    ns: list[float] = []
    for c in cells_for_cfg:
        strat = ((c.get("summary") or {}).get(strat_key) or {}).get(bucket)
        if strat:
            vals.append(float(strat["file_recall"]))
            ns.append(float(strat["n"]))
    if not vals:
        return None
    total_n = sum(ns)
    return sum(v * n for v, n in zip(vals, ns)) / total_n if total_n else None


def _render_strata_section(
    cfgs: list[tuple[str, int, int]],
    by_cfg: dict[tuple[str, int, int], list[dict]],
    title: str,
    note: str,
    strat_key: str,
    buckets: tuple[str, ...],
) -> list[str]:
    out: list[str] = ["", f"## {title}", "", note, ""]
    out.append("| method | budget | depth | " + " | ".join(buckets) + " |")
    out.append("|---|---:|---:|" + "---:|" * len(buckets))
    for cfg in cfgs:
        cs = by_cfg[cfg]
        row = [f"**{cfg[0]}**", str(cfg[1]), str(cfg[2])]
        for bucket in buckets:
            v = _avg_recall_for_bucket(cs, strat_key, bucket)
            row.append(f"{v:.3f}" if v is not None else "—")
        out.append("| " + " | ".join(row) + " |")
    return out


def render_stratification_tables(cells: list[dict]) -> str:
    """Recall stratified by |gold| bucket and by difficulty ratio.

    The most informative cross-cut: shows whether a method's headline number is
    driven by easy single-file instances or whether it actually scales with diff
    size and gold cardinality.
    """
    if not cells:
        return ""
    valid = [c for c in cells if c["method"] and c["budget"] is not None and c["test_set"]]
    if not valid:
        return ""
    have_gold_strata = any((c.get("summary") or {}).get("recall_by_gold_size") for c in valid)
    have_ratio_strata = any((c.get("summary") or {}).get("recall_by_difficulty_ratio") for c in valid)
    if not have_gold_strata and not have_ratio_strata:
        return ""

    by_cfg: dict[tuple[str, int, int], list[dict]] = defaultdict(list)
    for c in valid:
        by_cfg[(c["method"], c["budget"], c.get("depth") or -1)].append(c)
    cfgs = sorted(by_cfg.keys(), key=lambda k: (_method_sort_key(k[0]), int(k[1]), int(k[2])))

    out: list[str] = []
    if have_gold_strata:
        out.extend(
            _render_strata_section(
                cfgs,
                by_cfg,
                "Recall stratified by |gold| (file count)",
                "Buckets reflect how many files the gold patch touches; method must scale across all of them to be useful.",
                "recall_by_gold_size",
                ("1", "2-3", "4-7", "8-15", "16+"),
            )
        )
    if have_ratio_strata:
        out.extend(
            _render_strata_section(
                cfgs,
                by_cfg,
                "Recall stratified by difficulty ratio |gold|/|changed|",
                "Ratio≈1 means gold is the diff itself (trivial). Ratio>1 means real retrieval is needed.",
                "recall_by_difficulty_ratio",
                ("≤1.0", "1.0-1.5", "1.5-2.0", "2.0-3.0", "3.0+"),
            )
        )
    return "\n".join(out) + "\n" if out else ""


def render_gold_characterization(cells: list[dict]) -> str:
    """Per-test-set gold descriptors — emitted once per dataset, not per cell."""
    by_set: dict[str, dict] = {}
    for c in cells:
        ts = c["test_set"]
        if ts is None:
            continue
        gc = (c.get("summary") or {}).get("gold_characterization") or {}
        if not gc:
            continue
        if ts not in by_set:
            by_set[ts] = gc
    if not by_set:
        return ""
    out: list[str] = ["\n## Gold characterization (per dataset, from any cell)"]
    out.append("")
    out.append("| dataset | %single-file | %multi-file | %whole-file | %zero-gold |")
    out.append("|---|---:|---:|---:|---:|")
    for ts in sorted(by_set):
        gc = by_set[ts]
        out.append(
            f"| {ts} | {gc.get('single_file_pct', 0):.1f} | {gc.get('multi_file_pct', 0):.1f} | "
            f"{gc.get('whole_file_pct', 0):.1f} | {gc.get('zero_gold_pct', 0):.1f} |"
        )
    return "\n".join(out) + "\n"


def render_per_language_tables(cells: list[dict], top_n: int = 7) -> str:
    """Per-language breakdown for each (method, budget, depth) configuration.

    Picks the top-N languages by total instance count across all configurations,
    then prints a recall/F1/F2 row per (method, budget, depth) for each.
    """
    per_cfg = _aggregate_languages(cells)
    if not per_cfg:
        return ""

    lang_counts: dict[str, float] = defaultdict(float)
    for langs in per_cfg.values():
        for lang, agg in langs.items():
            lang_counts[lang] += agg["n"]
    top_langs = [lang for lang, _ in sorted(lang_counts.items(), key=lambda x: -x[1])[:top_n]]

    cfgs = sorted(per_cfg.keys(), key=lambda k: (_method_sort_key(k[0]), int(k[1]), int(k[2])))

    out: list[str] = ["\n## Per-language headline (top languages by instance count)"]
    out.append("")
    out.append("Each cell shows `recall / F1 / F2` for that (method, budget, depth) on that language.")
    out.append("")
    out.append("| config | " + " | ".join(top_langs) + " |")
    out.append("|---|" + "---|" * len(top_langs))
    for cfg in cfgs:
        langs = per_cfg[cfg]
        cells_md: list[str] = [f"**{cfg[0]}** b={cfg[1]} L={cfg[2]}"]
        for lang in top_langs:
            agg = langs.get(lang)
            if not agg or agg["n"] == 0:
                cells_md.append("—")
                continue
            cells_md.append(f"{agg['file_recall']:.3f} / {agg['f1']:.3f} / {agg['f2']:.3f}")
        out.append("| " + " | ".join(cells_md) + " |")
    return "\n".join(out) + "\n"


def write_csv(cells: list[dict], path: Path) -> None:
    fields = [
        "method",
        "budget",
        "depth",
        "test_set",
        "n_instances",
        "n_ok",
        "mean_file_recall",
        "mean_file_precision",
        "mean_file_f1",
        "mean_file_f2",
        "mean_file_f0_5",
        "mean_fragment_recall",
        "mean_fragment_precision",
        "mean_fragment_f1",
        "mean_line_f1",
        "mean_line_f1_given_file_hit",
        "n_with_fragment_gold",
        "recall_perfect_pct",
        "recall_zero_pct",
        "recall_partial_pct",
        "recall_std",
        "n_selected_p50",
        "n_selected_p95",
        "n_gold_p50",
        "fragment_count_p50",
        "fragment_count_p95",
        "mean_used_tokens",
        "tokens_p50",
        "tokens_p95",
        "tokens_p99",
        "mean_elapsed_seconds",
        "elapsed_p50",
        "elapsed_p95",
        "elapsed_p99",
        "git_sha",
        "started_at_utc",
    ]
    with path.open("w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fields)
        w.writeheader()
        for c in cells:
            s = c["summary"]
            file_recall = s.get("file_recall") or {}
            file_prec = s.get("file_precision") or {}
            fbeta = s.get("file_fbeta") or {}
            frag_block = s.get("fragment_recall") or {}
            frag_prec_block = s.get("fragment_precision") or {}
            frag_fbeta = s.get("fragment_fbeta") or {}
            line_block = s.get("line_f1") or {}
            line_cond = (line_block.get("conditional_on_file_hit") or {}) if line_block else {}
            tokens = s.get("used_tokens") or {}
            elapsed = s.get("elapsed_seconds") or {}
            rec_hist = file_recall.get("hist") or {}
            n_selected = s.get("n_selected") or {}
            n_gold = s.get("n_gold") or {}
            frag_count = s.get("fragment_count") or {}
            row = {
                "method": c["method"],
                "budget": c["budget"],
                "depth": c.get("depth"),
                "test_set": c["test_set"],
                "n_instances": s.get("n", c["n_instances"]),
                "n_ok": s.get("ok", 0),
                "mean_file_recall": file_recall.get("mean"),
                "mean_file_precision": file_prec.get("mean"),
                "mean_file_f1": (fbeta.get("f1") or {}).get("mean"),
                "mean_file_f2": (fbeta.get("f2") or {}).get("mean"),
                "mean_file_f0_5": (fbeta.get("f0.5") or {}).get("mean"),
                "mean_fragment_recall": frag_block.get("mean"),
                "mean_fragment_precision": frag_prec_block.get("mean"),
                "mean_fragment_f1": (frag_fbeta.get("f1") or {}).get("mean") if frag_fbeta else None,
                "mean_line_f1": line_block.get("mean"),
                "mean_line_f1_given_file_hit": line_cond.get("mean"),
                "n_with_fragment_gold": frag_block.get("n_with_gold"),
                "recall_perfect_pct": rec_hist.get("perfect_pct"),
                "recall_zero_pct": rec_hist.get("zero_pct"),
                "recall_partial_pct": rec_hist.get("partial_pct"),
                "recall_std": file_recall.get("std"),
                "n_selected_p50": n_selected.get("median"),
                "n_selected_p95": n_selected.get("p95"),
                "n_gold_p50": n_gold.get("median"),
                "fragment_count_p50": frag_count.get("median"),
                "fragment_count_p95": frag_count.get("p95"),
                "mean_used_tokens": tokens.get("mean"),
                "tokens_p50": tokens.get("median"),
                "tokens_p95": tokens.get("p95"),
                "tokens_p99": tokens.get("p99"),
                "mean_elapsed_seconds": elapsed.get("mean"),
                "elapsed_p50": elapsed.get("median"),
                "elapsed_p95": elapsed.get("p95"),
                "elapsed_p99": elapsed.get("p99"),
                "git_sha": (c["metadata"].get("git") or {}).get("sha"),
                "started_at_utc": c["metadata"].get("started_at_utc"),
            }
            w.writerow(row)


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--cells-dir", type=Path, required=True)
    p.add_argument("--sweep-id", type=str, required=True)
    p.add_argument("--out", type=Path, required=True)
    args = p.parse_args()

    args.out.mkdir(parents=True, exist_ok=True)
    cells = collect_cells(args.cells_dir)
    print(f"Collected {len(cells)} cells from {args.cells_dir}")

    grand = {
        "sweep_id": args.sweep_id,
        "n_cells": len(cells),
        "cells": [
            {
                "method": c["method"],
                "budget": c["budget"],
                "test_set": c["test_set"],
                "metadata": c["metadata"],
                "summary": c["summary"],
            }
            for c in cells
        ],
    }
    (args.out / "grand_summary.json").write_text(json.dumps(grand, indent=2, default=str))
    sweep_md = (
        render_sweep_table(cells)
        + render_headline_tables(cells)
        + render_pipeline_tables(cells)
        + render_stratification_tables(cells)
        + render_gold_characterization(cells)
        + render_per_language_tables(cells)
    )
    (args.out / "SWEEP_TABLE.md").write_text(sweep_md)
    write_csv(cells, args.out / "cell_index.csv")
    print(f"Wrote: {args.out / 'grand_summary.json'}")
    print(f"Wrote: {args.out / 'SWEEP_TABLE.md'}")
    print(f"Wrote: {args.out / 'cell_index.csv'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
