#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

BUDGETS = [2000, 4000, 8000]
MODES = ["hybrid", "ego", "ppr"]


def _clean_env() -> dict[str, str]:
    return {k: v for k, v in os.environ.items() if k != "DIFFCTX_SCORING"}


def run_at_budget(budget: int, mode: str, limit: int) -> None:
    print(f"  Running: budget={budget} mode={mode} limit={limit}...", flush=True)
    subprocess.run(
        [
            sys.executable,
            "benchmarks/contextbench_diffctx.py",
            "--limit",
            str(limit),
            "--budget",
            str(budget),
            "--scoring",
            mode,
        ],
        check=True,
        env=_clean_env(),
    )


def _aggregate_curve(out_dir: Path, limit: int) -> dict[str, list[dict[str, float]]]:
    curve: dict[str, list[dict[str, float]]] = {}
    for mode in MODES:
        curve[mode] = []
        for budget in BUDGETS:
            data_path = out_dir / f"cb_{mode}_n{limit}_b{budget}.json"
            if not data_path.exists():
                continue
            ok = [d for d in json.loads(data_path.read_text()) if d.get("status") == "ok"]
            if not ok:
                continue

            def avg(key: str) -> float:
                return round(sum(d[key] for d in ok) / len(ok), 3)

            curve[mode].append(
                {
                    "budget": budget,
                    "n": len(ok),
                    "nontrivial_file_recall": avg("nontrivial_file_recall"),
                    "file_recall": avg("file_recall"),
                    "line_recall": avg("line_recall"),
                }
            )
    return curve


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--limit", type=int, default=50)
    args = ap.parse_args()

    results_dir = Path("results")
    results_dir.mkdir(parents=True, exist_ok=True)

    for budget in BUDGETS:
        for mode in MODES:
            tag = f"cb_{mode}_n{args.limit}_b{budget}"
            output = results_dir / f"{tag}.json"
            if output.exists():
                print(f"  Skipping {output} (exists)", flush=True)
                continue
            run_at_budget(budget, mode, args.limit)

    curve = _aggregate_curve(results_dir, args.limit)
    curve_path = results_dir / "curve.json"
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
