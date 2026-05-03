"""Aggregate per-cell sweep artifacts into one summary JSON + markdown table.

Input layout (from `actions/download-artifact@v4` with pattern=cell-*):
    <cells-dir>/cell-<method>-b<budget>-<test_set>/
        metadata.json
        cell_summary.json
        <test_set>.checkpoint.jsonl
        <test_set>.json
        run.log
        system_info.log

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
        cells.append(
            {
                "artifact_dir": cell_root.name,
                "method": cell_info.get("method") or _parse_artifact(cell_root.name)[0],
                "budget": cell_info.get("budget") if cell_info.get("budget") is not None else _parse_artifact(cell_root.name)[1],
                "test_set": cell_info.get("test_set") or _parse_artifact(cell_root.name)[2],
                "metadata": meta,
                "summary": summary,
                "n_instances": len(rows),
                "instance_recall_values": [r.get("file_recall", 0.0) for r in rows],
            }
        )
    return cells


_ARTIFACT_RE = __import__("re").compile(r"^cell-(?P<method>[a-zA-Z0-9_]+)-b(?P<budget>-?\d+)-(?P<test_set>.+)$")


def _parse_artifact(name: str) -> tuple[str | None, int | None, str | None]:
    """Best-effort parse of a cell-<method>-b<budget>-<test_set> directory name.

    Used as a fallback when metadata.json was not produced (e.g., the cell
    crashed before the metadata step ran).
    """
    m = _ARTIFACT_RE.match(name)
    if not m:
        return (None, None, None)
    try:
        budget = int(m.group("budget"))
    except ValueError:
        budget = None
    return (m.group("method"), budget, m.group("test_set"))


_METHOD_ORDER = ["ppr", "ego", "hybrid", "bm25", "aider"]


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


def write_csv(cells: list[dict], path: Path) -> None:
    fields = [
        "method",
        "budget",
        "test_set",
        "n_instances",
        "n_ok",
        "mean_file_recall",
        "mean_file_precision",
        "mean_used_tokens",
        "mean_elapsed_seconds",
        "git_sha",
        "started_at_utc",
    ]
    with path.open("w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fields)
        w.writeheader()
        for c in cells:
            s = c["summary"]
            row = {
                "method": c["method"],
                "budget": c["budget"],
                "test_set": c["test_set"],
                "n_instances": s.get("n", c["n_instances"]),
                "n_ok": s.get("ok", 0),
                "mean_file_recall": (s.get("file_recall") or {}).get("mean"),
                "mean_file_precision": (s.get("file_precision") or {}).get("mean"),
                "mean_used_tokens": (s.get("used_tokens") or {}).get("mean"),
                "mean_elapsed_seconds": (s.get("elapsed_seconds") or {}).get("mean"),
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
    (args.out / "SWEEP_TABLE.md").write_text(render_sweep_table(cells))
    write_csv(cells, args.out / "cell_index.csv")
    print(f"Wrote: {args.out / 'grand_summary.json'}")
    print(f"Wrote: {args.out / 'SWEEP_TABLE.md'}")
    print(f"Wrote: {args.out / 'cell_index.csv'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
