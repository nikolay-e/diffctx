#!/usr/bin/env python3
from __future__ import annotations

import argparse
import os
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
from pathlib import Path

BUDGETS = [8000, 16000, 32000]
SCORINGS = ["ego", "ppr"]
DEFAULT_LIMIT = 1136
SEED = "42"
SHARED_CACHE = Path.home() / "treemapper_cb_cache"

CB_SCRIPT = "benchmarks/contextbench_diffctx.py"
LOO_SCRIPT = "benchmarks/loo_swebench.py"

# Paired per scoring: (cb,ego)+(loo,ego) run in parallel, then (cb,ppr)+(loo,ppr)
CB_GROUPS = [("ego", None), ("ppr", None), (None, "bm25")]  # (scoring, baseline)


def _ts() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M:%S")


def _prewarm(log_dir: Path) -> None:
    env = {**os.environ, "CB_REPOS_DIR": str(SHARED_CACHE)}
    for bench, script in [("cb", CB_SCRIPT)]:
        print(f">> {_ts()} | pre-warm {bench} ...", flush=True)
        cmd = [sys.executable, script, "--limit", "1", "--budget", "1000", "--seeds", SEED, "--scoring", "ego"]
        with open(log_dir / f"prewarm_{bench}.log", "w") as fh:
            rc = subprocess.run(cmd, env=env, stdout=fh, stderr=fh).returncode
        print(f">> {_ts()} | pre-warm {bench} {'OK' if rc == 0 else f'RC={rc}'}", flush=True)


def _run_cb_group(scoring: str | None, baseline: str | None, limit: int, log_dir: Path) -> list[str]:
    env = {**os.environ, "CB_REPOS_DIR": str(SHARED_CACHE)}
    if scoring:
        env["DIFFCTX_SCORING"] = scoring

    tag = "bm25" if baseline == "bm25" else scoring
    group_log = log_dir / f"cb_{tag}.log"
    summary_lines = []

    budgets = BUDGETS

    for budget in budgets:
        name = f"CB {tag} b={budget}"
        cmd = [sys.executable, CB_SCRIPT, "--limit", str(limit), "--budget", str(budget), "--seeds", SEED]
        if scoring:
            cmd += ["--scoring", scoring]
        if baseline:
            cmd += ["--scoring", "ego", "--baseline", baseline]

        t0 = time.time()
        with open(group_log, "a") as fh:
            fh.write(f"\n{'='*70}\n{name}\ncmd: {' '.join(cmd)}\n{'='*70}\n\n")
            fh.flush()
            rc = subprocess.run(cmd, env=env, stdout=fh, stderr=fh).returncode
        elapsed = time.time() - t0
        status = "OK" if rc == 0 else f"RC={rc}"
        line = f"{_ts()} | {name:35s} | {status:6s} | {elapsed:7.0f}s"
        summary_lines.append(line)
        print(f">> {line}", flush=True)

    return summary_lines


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--limit", type=int, default=DEFAULT_LIMIT)
    ap.add_argument("--skip-prewarm", action="store_true")
    args = ap.parse_args()

    run_ts = datetime.now(timezone.utc).strftime("%Y%m%d_%H%M%S")
    log_dir = Path("results") / f"local_max_{run_ts}"
    log_dir.mkdir(parents=True, exist_ok=True)
    main_log = log_dir / "summary.log"

    print(f"CB: ego x{len(BUDGETS)} + ppr x{len(BUDGETS)} + bm25 x1 = {len(BUDGETS) * 2 + 1} runs  [sequential]", flush=True)
    print(f"Shared cache: {SHARED_CACHE}", flush=True)
    print(f"Logs: {log_dir}/\n", flush=True)

    if not args.skip_prewarm:
        _prewarm(log_dir)

    all_lines: list[str] = []
    t_start = time.time()

    with ThreadPoolExecutor(max_workers=1) as pool:
        futures = {
            pool.submit(_run_cb_group, scoring, baseline, args.limit, log_dir): (scoring, baseline)
            for scoring, baseline in CB_GROUPS
        }
        for fut in as_completed(futures):
            scoring, baseline = futures[fut]
            try:
                all_lines.extend(fut.result())
            except Exception as exc:
                all_lines.append(f"ERROR {scoring or baseline}: {exc}")

    total = time.time() - t_start

    with open(main_log, "w") as f:
        f.write(f"=== local_max n={args.limit} budgets={BUDGETS} scorings={SCORINGS}+bm25 ===\n\n")
        for line in sorted(all_lines):
            f.write(line + "\n")
        f.write(f"\nTotal wall time: {total:.0f}s\n")

    print(f"\n{'='*70}\nSUMMARY\n{'='*70}")
    for line in sorted(all_lines):
        print(line)
    print(f"\nTotal wall time: {total:.0f}s")
    print(f"Summary log: {main_log}")


if __name__ == "__main__":
    main()
