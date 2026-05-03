#!/usr/bin/env python3
from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from dataclasses import dataclass

OPERATIONAL_PARAMS: list[tuple[str, float]] = [
    ("DIFFCTX_OP_PPR_ALPHA", 0.60),
    ("DIFFCTX_OP_PPR_FORWARD_BLEND", 0.40),
    ("DIFFCTX_OP_EGO_PER_HOP_DECAY", 1.0),
    ("DIFFCTX_OP_UTILITY_ETA", 0.20),
    ("DIFFCTX_OP_UTILITY_STRUCTURAL_BONUS_WEIGHT", 0.10),
    ("DIFFCTX_OP_UTILITY_R_CAP_SIGMA", 2.0),
    ("DIFFCTX_OP_UTILITY_PROXIMITY_DECAY", 0.30),
    ("DIFFCTX_OP_SELECTION_CORE_BUDGET_FRACTION", 0.70),
    ("DIFFCTX_OP_SELECTION_STOPPING_THRESHOLD", 0.08),
    ("DIFFCTX_OP_SELECTION_R_CAP_MIN", 0.01),
    ("DIFFCTX_OP_RESCUE_BUDGET_FRACTION", 0.05),
    ("DIFFCTX_OP_RESCUE_MIN_SCORE_PERCENTILE", 0.80),
    ("DIFFCTX_OP_FILTERING_PROXIMITY_HALF_DECAY", 50.0),
    ("DIFFCTX_OP_FILTERING_DEFINITION_PROXIMITY_HALF_DECAY", 5.0),
    ("DIFFCTX_OP_BOLTZMANN_CALIBRATION_TOLERANCE", 0.05),
]

PERTURBATION_FACTORS = [0.50, 0.75, 1.25, 1.50]
TOKEN_RE = re.compile(r"^([\d,]+)\s+tokens\b")
FRAGMENT_RE = re.compile(r"^ {2}(\S+):(\d+)-(\d+)")


@dataclass(frozen=True)
class RunResult:
    tokens: int
    fragments: frozenset[tuple[str, int, int]]


def parse_output(text: str) -> RunResult:
    tokens = 0
    fragments: set[tuple[str, int, int]] = set()
    for line in text.splitlines():
        if (m := TOKEN_RE.match(line)) and tokens == 0:
            tokens = int(m.group(1).replace(",", ""))
        if m := FRAGMENT_RE.match(line):
            path, start, end = m.group(1), int(m.group(2)), int(m.group(3))
            fragments.add((path, start, end))
    return RunResult(tokens=tokens, fragments=frozenset(fragments))


def jaccard(a: frozenset, b: frozenset) -> float:
    if not a and not b:
        return 1.0
    return len(a & b) / len(a | b)


def run_treemapper(repo: str, diff_range: str, budget: int, env_overrides: dict[str, str]) -> RunResult:
    env = os.environ.copy()
    env.update(env_overrides)
    proc = subprocess.run(
        [
            "treemapper",
            repo,
            "--diff",
            diff_range,
            "--budget",
            str(budget),
            "-f",
            "txt",
        ],
        env=env,
        capture_output=True,
        text=True,
        check=True,
    )
    return parse_output(proc.stdout + proc.stderr)


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--diff", default="HEAD~5..HEAD")
    p.add_argument("--budget", type=int, default=4096)
    p.add_argument("--repo", default=".")
    p.add_argument(
        "--params",
        help="Comma-separated env-var names to perturb (default: all 15)",
    )
    args = p.parse_args()

    selected = OPERATIONAL_PARAMS
    if args.params:
        wanted = set(args.params.split(","))
        selected = [(n, d) for (n, d) in OPERATIONAL_PARAMS if n in wanted]

    print(f"# Sensitivity analysis: {args.repo} {args.diff} budget={args.budget}")
    print()
    print("Baseline (all defaults)...", file=sys.stderr)
    baseline = run_treemapper(args.repo, args.diff, args.budget, {})
    print(f"# Baseline: tokens={baseline.tokens}  fragments={len(baseline.fragments)}")
    print()

    header = f"{'param':<55} {'factor':>6} {'value':>10} {'tokens':>8} {'Δ%':>7} {'jacc':>6}"
    print(header)
    print("-" * len(header))

    for name, default in selected:
        for factor in PERTURBATION_FACTORS:
            value = default * factor
            try:
                r = run_treemapper(args.repo, args.diff, args.budget, {name: f"{value:.6g}"})
            except subprocess.CalledProcessError as e:
                print(
                    f"{name:<55} {factor:>6.2f} {value:>10.4g} ERROR: {e.stderr[:80]}",
                    flush=True,
                )
                continue
            delta_pct = 100.0 * (r.tokens - baseline.tokens) / baseline.tokens if baseline.tokens else 0.0
            jacc = jaccard(baseline.fragments, r.fragments)
            print(
                f"{name:<55} {factor:>6.2f} {value:>10.4g} {r.tokens:>8d} {delta_pct:>+6.2f}% {jacc:>6.3f}",
                flush=True,
            )

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
