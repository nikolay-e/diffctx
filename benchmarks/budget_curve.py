#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

BUDGETS = [2000, 4000, 8000]
MODES = ["auto", "discover", "precise"]
DEFAULT_LIMIT = 50
DEFAULT_WORKERS = 11


def run_at_budget(budget: int, mode: str, output: Path, limit: int, workers: int) -> None:
    cmd = [
        sys.executable,
        "benchmarks/contextbench_diffctx.py",
        "--limit",
        str(limit),
        "--workers",
        str(workers),
        "--budget",
        str(budget),
        "--scoring",
        mode,
        "--output",
        str(output),
    ]
    print(f"  Running: budget={budget} mode={mode} limit={limit}...", flush=True)
    env = {k: v for k, v in os.environ.items() if k != "DIFFCTX_SCORING"}
    subprocess.run(cmd, check=True, env=env)


def main() -> None:
    import argparse

    ap = argparse.ArgumentParser()
    ap.add_argument("--limit", type=int, default=DEFAULT_LIMIT)
    ap.add_argument("--workers", type=int, default=DEFAULT_WORKERS)
    ap.add_argument("--output-dir", type=str, default="results/budget_curve")
    args = ap.parse_args()

    out_dir = Path(args.output_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    for budget in BUDGETS:
        for mode in MODES:
            output = out_dir / f"cb_{mode}_{budget}.json"
            if output.exists():
                print(f"  Skipping {output} (exists)", flush=True)
                continue
            run_at_budget(budget, mode, output, args.limit, args.workers)

    curve: dict[str, list[dict[str, float]]] = {}
    for mode in MODES:
        curve[mode] = []
        for budget in BUDGETS:
            data_path = out_dir / f"cb_{mode}_{budget}.json"
            if not data_path.exists():
                continue
            data = json.loads(data_path.read_text())
            ok = [d for d in data if d.get("status") == "ok"]
            if not ok:
                continue
            mean_nontrivial = sum(d["nontrivial_file_recall"] for d in ok) / len(ok)
            mean_file = sum(d["file_recall"] for d in ok) / len(ok)
            mean_line = sum(d["line_recall"] for d in ok) / len(ok)
            curve[mode].append(
                {
                    "budget": budget,
                    "n": len(ok),
                    "nontrivial_file_recall": round(mean_nontrivial, 3),
                    "file_recall": round(mean_file, 3),
                    "line_recall": round(mean_line, 3),
                }
            )

    curve_path = out_dir / "curve.json"
    curve_path.write_text(json.dumps(curve, indent=2))
    print(f"\nBudget curve saved to {curve_path}")

    for mode in MODES:
        print(f"\n{mode}:")
        for point in curve.get(mode, []):
            print(
                f"  budget={point['budget']:5d}  nontrivial={point['nontrivial_file_recall']:.3f}  file={point['file_recall']:.3f}  line={point['line_recall']:.3f}"
            )


if __name__ == "__main__":
    main()
