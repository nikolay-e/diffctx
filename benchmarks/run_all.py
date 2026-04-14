#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

BUDGETS = [1000, 2000, 4000, 6000, 8000, 16000, 32000]
LIMITS = [10, 20, 50, 100, 200, 500]
MODES = ["ego", "ppr"]


def _ts() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M:%S")


def _log(log_file: Path, line: str) -> None:
    print(f">> {line}")
    with open(log_file, "a") as f:
        f.write(line + "\n")


def _clean_env() -> dict[str, str]:
    return {k: v for k, v in os.environ.items() if k != "DIFFCTX_SCORING"}


def _summarize(output_file: Path, name: str, elapsed: float, log_file: Path) -> None:
    if not output_file.exists():
        return
    try:
        results = json.loads(output_file.read_text())
    except (json.JSONDecodeError, OSError):
        return

    ok = [r for r in results if r.get("status") == "ok"]
    if not ok:
        _log(log_file, f"{_ts()} | {name:45s} | n={len(results):3d} ok=0 | FAIL | {elapsed:.0f}s")
        return

    def avg(key: str) -> float:
        vals = [r[key] for r in ok if key in r]
        return sum(vals) / len(vals) if vals else 0.0

    _log(
        log_file,
        f"{_ts()} | {name:45s} | n={len(ok):3d} | "
        f"file R={avg('file_recall'):.3f} P={avg('file_precision'):.3f} | "
        f"nontrivial R={avg('nontrivial_file_recall'):.3f} | "
        f"line R={avg('line_recall'):.3f} nt={avg('line_recall_nontrivial'):.3f} | "
        f"def_cov={avg('def_coverage'):.3f} | "
        f"{elapsed:.0f}s",
    )


def run_bench(name: str, cmd: list[str], output_file: Path, log_file: Path) -> None:
    print(f"\n{'='*70}\n  {name}\n{'='*70}\n", flush=True)
    t0 = time.time()
    subprocess.run(cmd, env=_clean_env(), check=False)
    elapsed = time.time() - t0
    print(f"\n  [{name}] {elapsed:.0f}s\n", flush=True)
    _summarize(output_file, name, elapsed, log_file)


def _bench_cmd(py: str, limit: int, budget: int, scoring: str, baseline: str | None = None) -> list[str]:
    cmd = [
        py,
        "benchmarks/contextbench_diffctx.py",
        "--limit",
        str(limit),
        "--budget",
        str(budget),
    ]
    if baseline:
        cmd += ["--baseline", baseline]
    else:
        cmd += ["--scoring", scoring]
    return cmd


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--skip-baselines", action="store_true")
    args = ap.parse_args()

    results_dir = Path("results")
    run_ts = datetime.now(timezone.utc).strftime("%Y%m%d_%H%M%S")
    log_dir = results_dir / run_ts
    log_dir.mkdir(parents=True, exist_ok=True)
    log_file = log_dir / "log.txt"
    py = sys.executable

    _log(log_file, f"{_ts()} | === START ===")

    combos = sorted(
        [(n, b, m) for n in LIMITS for b in BUDGETS for m in MODES],
        key=lambda x: (x[0], x[1]),
    )

    for idx, (n, b, mode) in enumerate(combos, 1):
        tag = f"cb_{mode}_n{n}_b{b}"
        out_file = results_dir / f"{tag}.json"
        run_bench(
            f"[{idx}/{len(combos)}] {mode} n={n} b={b}",
            _bench_cmd(py, n, b, mode),
            out_file,
            log_file,
        )

    if not args.skip_baselines:
        for bl in ["patch_files", "bm25"]:
            out_file = results_dir / f"cb_baseline_{bl}.json"
            run_bench(
                f"baseline {bl}",
                _bench_cmd(py, 50, 8000, "", baseline=bl),
                out_file,
                log_file,
            )

    _log(log_file, f"{_ts()} | === DONE ===")
    print(f"\nLog: {log_file}")


if __name__ == "__main__":
    main()
