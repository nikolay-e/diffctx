#!/usr/bin/env python3
from __future__ import annotations

import sys
from pathlib import Path

from common import load_results as _load_raw
from stats import bootstrap_ci, paired_bootstrap_delta, wilcoxon_paired

METRICS = [
    "file_recall",
    "nontrivial_file_recall",
    "line_recall",
    "line_recall_nontrivial",
]


def load_results(path: Path) -> dict[str, dict]:
    return {r["id"]: r for r in _load_raw(path) if r.get("status") == "ok"}


def compare(after_path: Path, before_path: Path) -> None:
    after_by_id = load_results(after_path)
    before_by_id = load_results(before_path)
    common_ids = sorted(set(after_by_id) & set(before_by_id))

    if not common_ids:
        print("No common instances found.")
        return

    print(f"Paired instances: {len(common_ids)}")
    print(f"After:  {after_path.name}")
    print(f"Before: {before_path.name}")
    print()

    header = f"{'metric':30s}  {'before':18s}  {'after':18s}  {'delta':22s}  {'p_boot':>7s}  {'p_wilc':>7s}"
    print(header)
    print("-" * len(header))

    for metric in METRICS:
        before_vals = [before_by_id[iid][metric] for iid in common_ids if metric in before_by_id[iid]]
        after_vals = [after_by_id[iid][metric] for iid in common_ids if metric in after_by_id[iid]]

        if not before_vals or not after_vals or len(before_vals) != len(after_vals):
            continue

        b_mean, b_lo, b_hi = bootstrap_ci(before_vals)
        a_mean, a_lo, a_hi = bootstrap_ci(after_vals)
        delta = paired_bootstrap_delta(before_vals, after_vals)
        wilc = wilcoxon_paired(before_vals, after_vals)

        b_str = f"{b_mean:.3f} [{b_lo:.3f},{b_hi:.3f}]"
        a_str = f"{a_mean:.3f} [{a_lo:.3f},{a_hi:.3f}]"
        d_str = f"{delta['delta_mean']:+.3f} [{delta['ci_lo']:+.3f},{delta['ci_hi']:+.3f}]"
        pb_str = f"{delta['p_value']:.4f}"
        pw_str = f"{wilc['p_value']:.4f}"

        print(f"{metric:30s}  {b_str:18s}  {a_str:18s}  {d_str:22s}  {pb_str:>7s}  {pw_str:>7s}")


def main() -> None:
    if len(sys.argv) != 3:
        print(f"Usage: {sys.argv[0]} <after.json> <before.json>")
        sys.exit(1)
    compare(Path(sys.argv[1]), Path(sys.argv[2]))


if __name__ == "__main__":
    main()
