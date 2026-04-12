#!/usr/bin/env python3
from __future__ import annotations

import argparse
import os
import subprocess
import sys
import time
from pathlib import Path

MODES = ["auto", "discover", "precise"]


def run_bench(name: str, cmd: list[str], env: dict[str, str] | None = None) -> None:
    full_env = {k: v for k, v in os.environ.items() if k != "DIFFCTX_SCORING"}
    if env:
        full_env.update(env)
    print(f"\n{'='*70}")
    print(f"  {name}")
    print(f"{'='*70}\n", flush=True)
    t0 = time.time()
    subprocess.run(cmd, env=full_env, check=False)
    elapsed = time.time() - t0
    print(f"\n  [{name}] done in {elapsed:.0f}s\n", flush=True)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--limit", type=int, default=50)
    ap.add_argument("--workers", type=int, default=11)
    ap.add_argument("--budget", type=int, default=8000)
    ap.add_argument("--timeout", type=int, default=300)
    ap.add_argument("--output-dir", type=str, default="results")
    ap.add_argument("--skip-loo", action="store_true")
    ap.add_argument("--skip-cb", action="store_true")
    ap.add_argument("--modes", type=str, nargs="+", default=MODES, choices=MODES)
    args = ap.parse_args()

    out = Path(args.output_dir)
    out.mkdir(parents=True, exist_ok=True)

    py = sys.executable
    common_cb = ["--limit", str(args.limit), "--workers", str(args.workers), "--budget", str(args.budget)]
    common_loo = [
        "--limit",
        str(args.limit),
        "--workers",
        str(min(args.workers, 6)),
        "--budget",
        str(args.budget),
    ]

    if not args.skip_cb:
        for mode in args.modes:
            output = out / f"cb_{mode}.json"
            run_bench(
                f"CB {mode}",
                [py, "benchmarks/contextbench_diffctx.py", *common_cb, "--scoring", mode, "--output", str(output)],
            )

        run_bench(
            "CB baseline (patch_files)",
            [
                py,
                "benchmarks/contextbench_diffctx.py",
                *common_cb,
                "--baseline",
                "patch_files",
                "--output",
                str(out / "cb_baseline_patch_files.json"),
            ],
        )

    if not args.skip_loo:
        for mode in args.modes:
            output = out / f"loo_{mode}.json"
            run_bench(
                f"LOO {mode}",
                [py, "benchmarks/loo_swebench.py", *common_loo, "--scoring", mode, "--output", str(output)],
                env={"DIFFCTX_INSTANCE_TIMEOUT": str(args.timeout)},
            )

    print(f"\n{'='*70}")
    print("  ALL DONE")
    print(f"{'='*70}")
    print(f"Results in {out}/")
    for f in sorted(out.glob("*.json")):
        print(f"  {f.name} ({f.stat().st_size // 1024}KB)")


if __name__ == "__main__":
    main()
