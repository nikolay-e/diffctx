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


def _budget_sweep_records(cell_root: Path, meta: dict, method, depth, test_set) -> list[dict]:
    """Expand a multi-budget cell (issue #52 layout) into one record per budget.

    Layout: <cell>/<test_set>_budget_sweep/[L<depth>/]b<budget>.checkpoint.jsonl
    with a sibling per-budget summary at <cell>/cell_summary_b<budget>.json.
    """
    records: list[dict] = []
    for ckpt in sorted(cell_root.glob("*_budget_sweep/**/b*.checkpoint.jsonl")):
        m = _BUDGET_CKPT_RE.match(ckpt.name)
        if not m:
            continue
        budget = int(m.group("budget"))
        rows = _load_jsonl(ckpt)
        summary = _safe_load(cell_root / f"cell_summary_b{budget}.json") or {}
        records.append(
            {
                "artifact_dir": cell_root.name,
                "method": method,
                "budget": budget,
                "depth": depth,
                "test_set": test_set,
                "metadata": meta,
                "summary": summary,
                "n_instances": len(rows),
                "instance_recall_values": [r.get("file_recall", 0.0) for r in rows],
            }
        )
    return records


def collect_cells(cells_dir: Path) -> list[dict]:
    """Walk every cell-* artifact directory, return flat per-cell records.

    A single-budget cell contributes one record; a multi-budget cell
    (`--budgets` sweep, artifact `cell-<method>-bALL-...`) contributes one
    record per budget checkpoint so downstream tables stay keyed by
    (method, budget, depth, test_set) regardless of the producing layout.
    """
    cells: list[dict] = []
    for cell_root in sorted(cells_dir.iterdir()):
        if not cell_root.is_dir() or not cell_root.name.startswith("cell-"):
            continue
        meta = _safe_load(cell_root / "metadata.json") or {}
        cell_info = meta.get("cell") or {}
        parsed = _parse_artifact(cell_root.name)
        method = cell_info.get("method") or parsed[0]
        depth = cell_info.get("depth") if cell_info.get("depth") is not None else parsed[2]
        test_set = cell_info.get("test_set") or parsed[3]

        multi = _budget_sweep_records(cell_root, meta, method, depth, test_set)
        if multi:
            cells.extend(multi)
        else:
            cells.append(_single_budget_record(cell_root, meta, cell_info, parsed, method, depth, test_set))
    return cells


def _single_budget_record(cell_root: Path, meta: dict, cell_info: dict, parsed, method, depth, test_set) -> dict:
    summary = _safe_load(cell_root / "cell_summary.json") or {}
    # Find the per-instance checkpoint
    ckpts = sorted(cell_root.glob("*.checkpoint.jsonl"))
    rows = _load_jsonl(ckpts[0]) if ckpts else []
    meta_budget = cell_info.get("budget")
    budget = meta_budget if isinstance(meta_budget, int) else parsed[1]
    return {
        "artifact_dir": cell_root.name,
        "method": method,
        "budget": budget,
        "depth": depth,
        "test_set": test_set,
        "metadata": meta,
        "summary": summary,
        "n_instances": len(rows),
        "instance_recall_values": [r.get("file_recall", 0.0) for r in rows],
    }


# New artifact layout: cell-<method>-b<budget>-L<depth>-<test_set>.
# `bALL` marks a multi-budget cell (issue #52); its per-budget records are
# expanded from the b<budget>.checkpoint.jsonl files inside.
_ARTIFACT_RE_WITH_DEPTH = __import__("re").compile(
    r"^cell-(?P<method>[a-zA-Z0-9_]+)-b(?P<budget>-?\d+|ALL)-L(?P<depth>-?\d+)-(?P<test_set>.+)$"
)
# Legacy artifact layout: cell-<method>-b<budget>-<test_set> (no depth segment)
_ARTIFACT_RE_LEGACY = __import__("re").compile(r"^cell-(?P<method>[a-zA-Z0-9_]+)-b(?P<budget>-?\d+)-(?P<test_set>.+)$")

_BUDGET_CKPT_RE = __import__("re").compile(r"^b(?P<budget>-?\d+)\.checkpoint\.jsonl$")


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


def _depth_of(cell: dict) -> int:
    """Depth key for grouping. NOT `get("depth") or -1`: depth 0 is a real
    EGO radius and `or` coerced it to the -1 sentinel, mislabeling every
    L0 cell as depth-less in the rendered tables."""
    d = cell.get("depth")
    return d if isinstance(d, int) else -1


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
    return f"| n={n}" if fr is None else f"| {fr:.3f} (ok={ok}, n={n}, ITT)"


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


def _mean_of(cells_for_cfg, getter) -> float | None:
    vals = [v for v in (_cell_metric(c, getter) for c in cells_for_cfg) if v is not None]
    return sum(vals) / len(vals) if vals else None


def _fmt_sweep(v: float | None, ndigits: int = 4) -> str:
    return f"{v:.{ndigits}f}" if v is not None else "—"


def _render_fbeta_section(sorted_cfgs, by_cfg) -> list[str]:
    out = ["\n## Headline by F-beta (mean across datasets)", ""]
    out.append("| method | budget | depth | recall | precision | F0.5 | F1 | F2 | tokens p50 | tokens p95 |")
    out.append("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
    for cfg in sorted_cfgs:
        cs = by_cfg[cfg]
        recall = _mean_of(cs, lambda s: s["file_recall"]["mean"])
        prec = _mean_of(cs, lambda s: s["file_precision"]["mean"])
        f1 = _mean_of(cs, lambda s: s["file_fbeta"]["f1"]["mean"])
        f2 = _mean_of(cs, lambda s: s["file_fbeta"]["f2"]["mean"])
        f05 = _mean_of(cs, lambda s: s["file_fbeta"]["f0.5"]["mean"])
        tk_p50 = _mean_of(cs, lambda s: s["used_tokens"]["median"])
        tk_p95 = _mean_of(cs, lambda s: s["used_tokens"]["p95"])
        out.append(
            f"| **{cfg[0]}** | {cfg[1]} | {cfg[2]} | {_fmt_sweep(recall)} | {_fmt_sweep(prec)} | "
            f"{_fmt_sweep(f05)} | {_fmt_sweep(f1)} | {_fmt_sweep(f2)} | "
            f"{_fmt_sweep(tk_p50, 0)} | {_fmt_sweep(tk_p95, 0)} |"
        )
    return out


def _render_robustness_section(sorted_cfgs, by_cfg) -> list[str]:
    out = ["\n## Robustness — recall distribution (mean across datasets)", ""]
    out.append("| method | budget | depth | %perfect | %zero | %partial | recall std |")
    out.append("|---|---:|---:|---:|---:|---:|---:|")
    for cfg in sorted_cfgs:
        cs = by_cfg[cfg]
        perfect = _mean_of(cs, lambda s: s["file_recall"]["hist"]["perfect_pct"])
        zero = _mean_of(cs, lambda s: s["file_recall"]["hist"]["zero_pct"])
        partial = _mean_of(cs, lambda s: s["file_recall"]["hist"]["partial_pct"])
        std = _mean_of(cs, lambda s: s["file_recall"]["std"])
        out.append(
            f"| **{cfg[0]}** | {cfg[1]} | {cfg[2]} | {_fmt_sweep(perfect, 1)} | {_fmt_sweep(zero, 1)} | "
            f"{_fmt_sweep(partial, 1)} | {_fmt_sweep(std, 3)} |"
        )
    return out


def _render_latency_section(sorted_cfgs, by_cfg) -> list[str]:
    out = ["\n## Latency — elapsed_seconds across datasets", ""]
    out.append("| method | budget | depth | mean | p50 | p95 | p99 |")
    out.append("|---|---:|---:|---:|---:|---:|---:|")
    for cfg in sorted_cfgs:
        cs = by_cfg[cfg]
        mean = _mean_of(cs, lambda s: s["elapsed_seconds"]["mean"])
        p50 = _mean_of(cs, lambda s: s["elapsed_seconds"]["median"])
        p95 = _mean_of(cs, lambda s: s["elapsed_seconds"]["p95"])
        p99 = _mean_of(cs, lambda s: s["elapsed_seconds"]["p99"])
        out.append(
            f"| **{cfg[0]}** | {cfg[1]} | {cfg[2]} | {_fmt_sweep(mean, 2)} | {_fmt_sweep(p50, 2)} | "
            f"{_fmt_sweep(p95, 2)} | {_fmt_sweep(p99, 2)} |"
        )
    return out


def _render_cardinality_section(sorted_cfgs, by_cfg, cardinality_present: bool) -> list[str]:
    out = ["\n## Selection cardinality (files / fragments)", ""]
    if cardinality_present:
        out.append("| method | budget | depth | n_selected p50 | n_selected p95 | n_gold p50 |")
        out.append("|---|---:|---:|---:|---:|---:|")
        for cfg in sorted_cfgs:
            cs = by_cfg[cfg]
            n_sel_p50 = _mean_of(cs, lambda s: s["n_selected"]["median"])
            n_sel_p95 = _mean_of(cs, lambda s: s["n_selected"]["p95"])
            n_gold_p50 = _mean_of(cs, lambda s: s["n_gold"]["median"])
            out.append(
                f"| **{cfg[0]}** | {cfg[1]} | {cfg[2]} | "
                f"{_fmt_sweep(n_sel_p50, 1)} | {_fmt_sweep(n_sel_p95, 1)} | {_fmt_sweep(n_gold_p50, 1)} |"
            )
    else:
        out.append("| method | budget | depth | fragment_count p50 | p95 |")
        out.append("|---|---:|---:|---:|---:|")
        for cfg in sorted_cfgs:
            cs = by_cfg[cfg]
            fc_p50 = _mean_of(cs, lambda s: s["fragment_count"]["median"])
            fc_p95 = _mean_of(cs, lambda s: s["fragment_count"]["p95"])
            out.append(f"| **{cfg[0]}** | {cfg[1]} | {cfg[2]} | {_fmt_sweep(fc_p50, 1)} | {_fmt_sweep(fc_p95, 1)} |")
    return out


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
        by_cfg[(c["method"], c["budget"], _depth_of(c))].append(c)

    sorted_cfgs = sorted(by_cfg.keys(), key=lambda k: (_method_sort_key(k[0]), int(k[1]), int(k[2])))

    out: list[str] = []
    out.extend(_render_fbeta_section(sorted_cfgs, by_cfg))
    out.extend(_render_robustness_section(sorted_cfgs, by_cfg))
    out.extend(_render_latency_section(sorted_cfgs, by_cfg))

    cardinality_present = any((c.get("summary") or {}).get("n_selected") for c in valid)
    fragment_present = any((c.get("summary") or {}).get("fragment_count") for c in valid)
    if cardinality_present or fragment_present:
        out.extend(_render_cardinality_section(sorted_cfgs, by_cfg, cardinality_present))

    return "\n".join(out) + "\n"


def _accumulate_language_stats(per_lang: dict, bucket: dict) -> None:
    for lang, agg in per_lang.items():
        cur = bucket.setdefault(lang, {"n": 0.0, "recall_sum": 0.0, "precision_sum": 0.0, "f1_sum": 0.0, "f2_sum": 0.0})
        n = float(agg.get("n", 0))
        cur["n"] += n
        cur["recall_sum"] += float(agg.get("file_recall", 0.0)) * n
        cur["precision_sum"] += float(agg.get("file_precision", 0.0)) * n
        cur["f1_sum"] += float(agg.get("f1", 0.0)) * n
        cur["f2_sum"] += float(agg.get("f2", 0.0)) * n


def _finalize_language_stat(v: dict) -> dict[str, float]:
    n = v["n"]
    return {
        "n": n,
        "file_recall": v["recall_sum"] / n if n else 0.0,
        "file_precision": v["precision_sum"] / n if n else 0.0,
        "f1": v["f1_sum"] / n if n else 0.0,
        "f2": v["f2_sum"] / n if n else 0.0,
    }


def _aggregate_languages(cells: list[dict]) -> dict[tuple[str, int, int], dict[str, dict[str, float]]]:
    out: dict[tuple[str, int, int], dict[str, dict[str, float]]] = {}
    for c in cells:
        m, b, d = c["method"], c["budget"], _depth_of(c)
        if m is None or b is None:
            continue
        cfg = (m, b, d)
        per_lang = (c.get("summary") or {}).get("by_language") or {}
        if not per_lang:
            continue
        bucket = out.setdefault(cfg, {})
        _accumulate_language_stats(per_lang, bucket)
    return {cfg: {lang: _finalize_language_stat(v) for lang, v in langs.items()} for cfg, langs in out.items()}


def _has_latency_field(valid: list[dict], field: str) -> bool:
    for c in valid:
        lb = (c.get("summary") or {}).get("latency_breakdown") or {}
        if field in lb:
            return True
    return False


def _fmt_latency(v: float | None, ndigits: int = 1) -> str:
    return f"{v:.{ndigits}f}" if v is not None else "—"


def _render_latency_breakdown_section(cfgs, by_cfg) -> list[str]:
    out = ["\n## Pipeline latency breakdown (median, ms)", ""]
    out.append("| method | budget | depth | parse | discover | tokenize | scoring | selection |")
    out.append("|---|---:|---:|---:|---:|---:|---:|---:|")
    for cfg in cfgs:
        cs = by_cfg[cfg]
        parse = _mean_of(cs, lambda s: s["latency_breakdown"]["parse_changed_ms"]["median"])
        discov = _mean_of(cs, lambda s: s["latency_breakdown"]["discovery_ms"]["median"])
        token = _mean_of(cs, lambda s: s["latency_breakdown"]["tokenization_ms"]["median"])
        scoring = _mean_of(cs, lambda s: s["latency_breakdown"]["scoring_ms"]["median"])
        selection = _mean_of(cs, lambda s: s["latency_breakdown"]["selection_ms"]["median"])
        out.append(
            f"| **{cfg[0]}** | {cfg[1]} | {cfg[2]} | "
            f"{_fmt_latency(parse)} | {_fmt_latency(discov)} | {_fmt_latency(token)} | "
            f"{_fmt_latency(scoring)} | {_fmt_latency(selection)} |"
        )
    return out


def _render_graph_size_section(cfgs, by_cfg) -> list[str]:
    out = ["\n## Graph size — edges and pushes (median per instance)", ""]
    out.append("| method | budget | depth | candidates | edges | edges_dropped | nodes_capped | ppr_fwd | ppr_bwd |")
    out.append("|---|---:|---:|---:|---:|---:|---:|---:|---:|")
    for cfg in cfgs:
        cs = by_cfg[cfg]
        cand = _mean_of(cs, lambda s: s["latency_breakdown"]["candidate_count"]["median"])
        edges = _mean_of(cs, lambda s: s["latency_breakdown"]["edge_count"]["median"])
        dropped = _mean_of(cs, lambda s: s["latency_breakdown"]["edges_dropped_by_cap"]["median"])
        nodes_capped = _mean_of(cs, lambda s: s["latency_breakdown"]["nodes_capped"]["median"])
        ppr_fwd = _mean_of(cs, lambda s: s["latency_breakdown"]["ppr_forward_pushes"]["median"])
        ppr_bwd = _mean_of(cs, lambda s: s["latency_breakdown"]["ppr_backward_pushes"]["median"])
        out.append(
            f"| **{cfg[0]}** | {cfg[1]} | {cfg[2]} | "
            f"{_fmt_latency(cand, 0)} | {_fmt_latency(edges, 0)} | {_fmt_latency(dropped, 0)} | "
            f"{_fmt_latency(nodes_capped, 0)} | {_fmt_latency(ppr_fwd, 0)} | {_fmt_latency(ppr_bwd, 0)} |"
        )
    return out


def render_pipeline_tables(cells: list[dict]) -> str:
    """Latency breakdown + graph stats — only emitted when at least one cell has them."""
    if not cells:
        return ""
    valid = [c for c in cells if c["method"] and c["budget"] is not None and c["test_set"]]
    by_cfg: dict[tuple[str, int, int], list[dict]] = defaultdict(list)
    for c in valid:
        by_cfg[(c["method"], c["budget"], _depth_of(c))].append(c)

    cfgs = sorted(by_cfg.keys(), key=lambda k: (_method_sort_key(k[0]), int(k[1]), int(k[2])))

    out: list[str] = []
    if _has_latency_field(valid, "scoring_ms") or _has_latency_field(valid, "discovery_ms"):
        out.extend(_render_latency_breakdown_section(cfgs, by_cfg))
    if _has_latency_field(valid, "edge_count"):
        out.extend(_render_graph_size_section(cfgs, by_cfg))

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
        by_cfg[(c["method"], c["budget"], _depth_of(c))].append(c)
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


def _or_empty(mapping: dict, key: str) -> dict:
    return mapping.get(key) or {}


def _csv_blocks(s: dict) -> dict[str, dict]:
    file_recall = _or_empty(s, "file_recall")
    line_block = _or_empty(s, "line_f1")
    return {
        "file_recall": file_recall,
        "file_precision": _or_empty(s, "file_precision"),
        "fbeta": _or_empty(s, "file_fbeta"),
        "frag": _or_empty(s, "fragment_recall"),
        "frag_prec": _or_empty(s, "fragment_precision"),
        "frag_fbeta": _or_empty(s, "fragment_fbeta"),
        "line": line_block,
        "line_cond": _or_empty(line_block, "conditional_on_file_hit") if line_block else {},
        "tokens": _or_empty(s, "used_tokens"),
        "elapsed": _or_empty(s, "elapsed_seconds"),
        "rec_hist": _or_empty(file_recall, "hist"),
        "n_selected": _or_empty(s, "n_selected"),
        "n_gold": _or_empty(s, "n_gold"),
        "frag_count": _or_empty(s, "fragment_count"),
    }


def _csv_row_for_cell(c: dict) -> dict:
    s = c["summary"]
    b = _csv_blocks(s)
    return {
        "method": c["method"],
        "budget": c["budget"],
        "depth": c.get("depth"),
        "test_set": c["test_set"],
        "n_instances": s.get("n", c["n_instances"]),
        "n_ok": s.get("ok", 0),
        "mean_file_recall": b["file_recall"].get("mean"),
        "mean_file_precision": b["file_precision"].get("mean"),
        "mean_file_f1": _or_empty(b["fbeta"], "f1").get("mean"),
        "mean_file_f2": _or_empty(b["fbeta"], "f2").get("mean"),
        "mean_file_f0_5": _or_empty(b["fbeta"], "f0.5").get("mean"),
        "mean_fragment_recall": b["frag"].get("mean"),
        "mean_fragment_precision": b["frag_prec"].get("mean"),
        "mean_fragment_f1": _or_empty(b["frag_fbeta"], "f1").get("mean") if b["frag_fbeta"] else None,
        "mean_line_f1": b["line"].get("mean"),
        "mean_line_f1_given_file_hit": b["line_cond"].get("mean"),
        "n_with_fragment_gold": b["frag"].get("n_with_gold"),
        "recall_perfect_pct": b["rec_hist"].get("perfect_pct"),
        "recall_zero_pct": b["rec_hist"].get("zero_pct"),
        "recall_partial_pct": b["rec_hist"].get("partial_pct"),
        "recall_std": b["file_recall"].get("std"),
        "n_selected_p50": b["n_selected"].get("median"),
        "n_selected_p95": b["n_selected"].get("p95"),
        "n_gold_p50": b["n_gold"].get("median"),
        "fragment_count_p50": b["frag_count"].get("median"),
        "fragment_count_p95": b["frag_count"].get("p95"),
        "mean_used_tokens": b["tokens"].get("mean"),
        "tokens_p50": b["tokens"].get("median"),
        "tokens_p95": b["tokens"].get("p95"),
        "tokens_p99": b["tokens"].get("p99"),
        "mean_elapsed_seconds": b["elapsed"].get("mean"),
        "elapsed_p50": b["elapsed"].get("median"),
        "elapsed_p95": b["elapsed"].get("p95"),
        "elapsed_p99": b["elapsed"].get("p99"),
        "git_sha": _or_empty(c["metadata"], "git").get("sha"),
        "started_at_utc": c["metadata"].get("started_at_utc"),
    }


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
            w.writerow(_csv_row_for_cell(c))


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
