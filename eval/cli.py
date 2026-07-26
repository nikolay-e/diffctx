#!/usr/bin/env python3
from __future__ import annotations

import sys

SUBCOMMANDS = {
    "run": ("workflows.run", "Run an evaluation manifest"),
    "run-final": ("workflows.run_final", "Run the frozen final evaluation"),
    "calibrate": ("workflows.calibrate", "Calibrate parameters on a frozen split"),
    "describe-dataset": ("datasets.describe", "Describe a dataset or manifest"),
    "build-splits": ("datasets.build_splits", "Build frozen evaluation splits"),
    "pin-revisions": ("datasets.pin_revisions", "Pin external dataset revisions"),
    "select-final": ("workflows.select_final", "Select the final calibrated candidate"),
    "equivalence": ("analysis.equivalence_gate", "Check two runs for bit equivalence"),
    "cell-metrics": ("analysis.cell_metrics", "Summarize a sweep checkpoint"),
    "aggregate-sweep": ("analysis.aggregate_sweep", "Aggregate sweep cells"),
    "stratified-analysis": ("analysis.stratified_analysis", "Run stratified statistical analysis"),
    "render-comparison": ("analysis.render_comparison", "Render a method comparison report"),
    "backfill-checkpoints": ("workflows.backfill_checkpoints", "Backfill legacy sweep checkpoints"),
    "verify-dcbench": ("datasets.dcbench.verify_instances", "Verify local dcbench instances"),
    "generate-dcbench-candidates": ("datasets.dcbench.gen_candidates", "Generate dcbench annotation candidates"),
    "extract-dcbench-commits": ("datasets.dcbench.extract_toanalyze_commits", "Create dcbench instances from commit lists"),
    "annotate-dcbench-hops": ("datasets.dcbench.annotate_hops", "Annotate graph-hop metadata"),
    "convert-legacy-labels": ("datasets.dcbench.convert_legacy_labels", "Convert legacy real-world labels"),
    "cb": ("workflows.contextbench", "ContextBench evaluation (--forensic for diagnostics)"),
    "loo": ("workflows.leave_one_out", "Leave-One-Out evaluation"),
    "compare": ("analysis.compare_runs", "A/B comparison of two result files"),
    "curve": ("analysis.budget_curve", "Budget curve analysis across budgets/modes"),
    "aggregate": ("analysis.aggregate_seeds", "Aggregate results across seeds"),
}


def _print_usage(exit_code: int = 1) -> None:
    print("usage: python -m eval <command> [args]\n")
    print("commands:")
    for name, (_, desc) in SUBCOMMANDS.items():
        print(f"  {name:12s}  {desc}")
    sys.exit(exit_code)


def main() -> None:
    if len(sys.argv) < 2 or sys.argv[1] in ("-h", "--help"):
        _print_usage(0)

    cmd = sys.argv[1]
    if cmd not in SUBCOMMANDS:
        print(f"unknown command: {cmd}")
        _print_usage()

    if cmd == "cb" and "--forensic" in sys.argv:
        sys.argv.remove("--forensic")
        module_name = "workflows.forensic"
    else:
        module_name, _ = SUBCOMMANDS[cmd]

    sys.argv = [f"eval {cmd}", *sys.argv[2:]]

    import importlib

    mod = importlib.import_module(f"eval.{module_name}")
    mod.main()


if __name__ == "__main__":
    main()
