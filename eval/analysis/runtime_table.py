"""The Appendix-B runtime/memory telemetry table, from sweep checkpoints (#168).

Paper v2 promised per-instance latency percentiles, peak RSS, node/edge
counters and per-phase shares "reported from the consolidated rerun's logs" and
never delivered them. The logs carry everything needed
(`extra.latency_breakdown` per ok row), so the table is a parse, not a rerun.

Rows cover instances that produced output; a cell's non-ok remainder is
reported as its own column rather than silently shaping the percentiles.

CLI:
    python -m eval.analysis.runtime_table --runs results/sweep_v2_local > table.md
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

PHASES = [
    "parse_changed_ms",
    "universe_walk_ms",
    "discovery_ms",
    "parse_discovered_ms",
    "tokenization_ms",
    "graph_build_ms",
    "scoring_ms",
    "selection_ms",
]


def pct(vals: list[float], q: float) -> float:
    if not vals:
        return float("nan")
    s = sorted(vals)
    return s[min(len(s) - 1, int(q * len(s)))]


def _load_rows(cp: Path) -> tuple[list[tuple[float, dict]], int]:
    rows = []
    n_total = 0
    for line in cp.read_text().splitlines():
        if not line.strip():
            continue
        n_total += 1
        r = json.loads(line)
        lb = (r.get("extra") or {}).get("latency_breakdown")
        total = (r.get("extra") or {}).get("latency_total_ms")
        if lb and total is not None:
            rows.append((float(total), lb))
    return rows, n_total


def _table_line(cell: Path, cp: Path, rows: list[tuple[float, dict]], n_total: int) -> str:
    totals = [t for t, _ in rows]
    rss = [lb.get("peak_rss_bytes", 0) / 1e6 for _, lb in rows if lb.get("peak_rss_bytes")]
    edges = [lb.get("edge_count", 0) for _, lb in rows]
    pre = [lb.get("edges_before_cap", 0) for _, lb in rows]
    phase_sum = {p: sum(lb.get(p, 0.0) for _, lb in rows) for p in PHASES}
    grand = sum(phase_sum.values()) or 1.0
    top = sorted(phase_sum.items(), key=lambda kv: -kv[1])[:3]
    top_s = ", ".join(f"{k.removesuffix('_ms')} {v / grand:.0%}" for k, v in top)
    bench = cp.name.replace(".checkpoint.jsonl", "")
    return (
        f"| {cell.name} | {bench} | {len(rows)}/{n_total} "
        f"| {pct(totals, 0.5):.0f}/{pct(totals, 0.9):.0f}/{pct(totals, 0.99):.0f} "
        f"| {pct(rss, 0.5):.0f}/{pct(rss, 0.99):.0f} "
        f"| {pct(edges, 0.5):.0f} ({pct(pre, 0.5):.0f}) "
        f"| {top_s} |"
    )


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", required=True)
    args = ap.parse_args()

    root = Path(args.runs)
    print("# Appendix B — runtime and memory telemetry (#168)\n")
    print(f"Source: per-instance `latency_breakdown` in `{root}` checkpoints; ok rows only.\n")
    print(
        "Percentiles are over the ok rows alone; the n column carries the "
        "denominator so a timeout-heavy cell cannot pose as a fast one.\n"
    )
    print(
        "| cell | benchmark | n ok/rows | total p50/p90/p99 ms | peak RSS p50/p99 MB | "
        "edges p50 (pre-cap p50) | dominant phases (share of summed phase time) |"
    )
    print("|---|---|---:|---|---|---|---|")

    for cell in sorted(d for d in root.iterdir() if d.is_dir()):
        for cp in sorted(cell.glob("*.checkpoint.jsonl")):
            rows, n_total = _load_rows(cp)
            if rows:
                print(_table_line(cell, cp, rows, n_total))


if __name__ == "__main__":
    main()
