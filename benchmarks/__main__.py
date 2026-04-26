#!/usr/bin/env python3
from __future__ import annotations

import sys

SUBCOMMANDS = {
    "cb": ("contextbench_diffctx", "ContextBench evaluation (--forensic for diagnostics)"),
    "loo": ("loo_swebench", "Leave-One-Out evaluation"),
    "compare": ("compare_runs", "A/B comparison of two result files"),
    "curve": ("budget_curve", "Budget curve analysis across budgets/modes"),
    "aggregate": ("aggregate_seeds", "Aggregate results across seeds"),
}


def _print_usage() -> None:
    print("usage: python -m benchmarks <command> [args]\n")
    print("commands:")
    for name, (_, desc) in SUBCOMMANDS.items():
        print(f"  {name:12s}  {desc}")
    sys.exit(1)


def main() -> None:
    if len(sys.argv) < 2 or sys.argv[1] in ("-h", "--help"):
        _print_usage()

    cmd = sys.argv[1]
    if cmd not in SUBCOMMANDS:
        print(f"unknown command: {cmd}")
        _print_usage()

    if cmd == "cb" and "--forensic" in sys.argv:
        sys.argv.remove("--forensic")
        module_name = "forensic_contextbench"
    else:
        module_name, _ = SUBCOMMANDS[cmd]

    sys.argv = [f"benchmarks {cmd}", *sys.argv[2:]]

    import importlib

    mod = importlib.import_module(f"benchmarks.{module_name}")
    mod.main()


if __name__ == "__main__":
    main()
