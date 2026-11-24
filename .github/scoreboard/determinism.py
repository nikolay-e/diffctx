#!/usr/bin/env python3
"""The 30 cells run repeatedly under different RAYON_NUM_THREADS, every answer byte-compared with `cmp`.

  determinism.py --bin PATH --fixture DIR --out DIR [--sha SHA]

Run 1..5 use 1, 2, 4, 8 threads and the default (the variable unset), each with a cold token cache of
its own; run 6 repeats the default on run 5's warm cache. `latency` and `provenance` are removed from
every answer as cells.py does; the removed provenance is compared apart and only reported.
Writes <out>/determinism.md; exit 1 when any normalized answer differs from run 1's.
"""

import argparse
import filecmp
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
RUNS = [("t1", "1"), ("t2", "2"), ("t4", "4"), ("t8", "8"), ("default", None), ("default-warm", None)]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--fixture", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--sha", default="")
    a = ap.parse_args()
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    for label, threads in RUNS:
        cache = out / f"cache-{'default' if label.startswith('default') else label}"
        env = {k: v for k, v in os.environ.items() if k != "RAYON_NUM_THREADS"}
        env["DIFFCTX_TOKEN_CACHE_DIR"] = str(cache)
        if threads:
            env["RAYON_NUM_THREADS"] = threads
        cmd = [
            sys.executable,
            "-I",
            str(HERE / "cells.py"),
            "run",
            "--bin",
            a.bin,
            "--fixture",
            a.fixture,
            "--out",
            str(out / label),
        ]
        print(f"run {label} (RAYON_NUM_THREADS={threads or 'unset'})", flush=True)
        subprocess.run(cmd, env=env, check=True)
    first = out / RUNS[0][0]
    names = sorted(p.name for p in first.iterdir() if p.suffix in (".json", ".err") and ".provenance" not in p.name)
    rows, differing, provenance = [], 0, 0
    for name in names:
        cells = [(label, out / label / name) for label, _ in RUNS]
        bad = [label for label, p in cells[1:] if not p.exists() or not filecmp.cmp(cells[0][1], p, shallow=False)]
        if name.endswith(".json"):
            prov = name[:-5] + ".provenance.json"
            pbad = [label for label, _ in RUNS[1:] if (out / label / prov).read_bytes() != (first / prov).read_bytes()]
            provenance += bool(pbad)
        else:
            pbad = []
        differing += bool(bad)
        rows.append(
            f"| {name[:-5] if name.endswith('.json') else name} | {len(RUNS) - len(bad)}/{len(RUNS)} | {', '.join(bad) or '-'} | {', '.join(pbad) or '-'} |"
        )
    sha = f" at {a.sha}" if a.sha else ""
    lines = [
        f"# Determinism{sha}: {len(names)} cells x {len(RUNS)} runs",
        "",
        "runs: " + ", ".join(f"{label} (RAYON_NUM_THREADS={threads or 'unset'})" for label, threads in RUNS),
        "",
        f"cells whose answer differs from run 1's (cmp, latency and provenance removed): {differing}",
        f"cells whose removed provenance differs (reported only): {provenance}",
        "",
        "| cell | identical to run 1 | differs in | provenance differs in |",
        "|---|---:|---|---|",
        *rows,
    ]
    (out / "determinism.md").write_text("\n".join(lines) + "\n")
    print("\n".join(lines[:6]))
    sys.exit(1 if differing else 0)


if __name__ == "__main__":
    main()
