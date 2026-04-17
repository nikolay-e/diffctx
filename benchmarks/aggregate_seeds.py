#!/usr/bin/env python3
from __future__ import annotations

import statistics
import sys
from pathlib import Path

from common import load_results

METRICS = ["file_recall", "file_precision", "nontrivial_file_recall", "line_recall", "line_recall_nontrivial"]


def main() -> None:
    paths = [Path(a) for a in sys.argv[1:] if Path(a).exists()]
    if not paths:
        print("Usage: aggregate_seeds.py results/cb_ego_n50_b16000_s*.json")
        sys.exit(1)

    seed_avgs: dict[str, list[float]] = {m: [] for m in METRICS}

    for path in sorted(paths):
        results = load_results(path)
        ok = [r for r in results if r.get("status") == "ok"]
        if not ok:
            continue
        print(f"{path.name}: {len(ok)} ok", end="")
        for m in METRICS:
            vals = [r[m] for r in ok if m in r]
            if vals:
                avg = sum(vals) / len(vals)
                seed_avgs[m].append(avg)
                print(f"  {m.split('_')[-1]}={avg:.3f}", end="")
        print()

    if not any(seed_avgs.values()):
        return

    print(f"\n{'='*60}")
    print(f"AGGREGATE ({len(paths)} seeds)")
    print(f"{'='*60}")
    for m in METRICS:
        vals = seed_avgs[m]
        if not vals:
            continue
        mean = statistics.mean(vals)
        std = statistics.stdev(vals) if len(vals) > 1 else 0.0
        print(f"  {m:30s}: {mean:.3f} \u00b1 {std:.3f}")


if __name__ == "__main__":
    main()
