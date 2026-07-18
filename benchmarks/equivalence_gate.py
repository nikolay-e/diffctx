"""Bit-equivalence gate between two eval runs.

Compares per-instance results of two `run_final_eval` output directories
(same manifests, same params, different code builds). Performance
refactors must pass this gate before entering an evaluation sweep:
identical selected-fragment file sets, identical used_tokens, recall
delta below 1e-9, identical statuses.

CLI:
    python -m benchmarks.equivalence_gate --a /tmp/run_old --b /tmp/run_new
Exit code 0 = equivalent, 1 = mismatches found (printed).
"""

from __future__ import annotations

import argparse
import glob
import json
from pathlib import Path


def load_run(d: Path) -> dict[str, dict]:
    out: dict[str, dict] = {}
    for f in glob.glob(str(d / "**" / "*.checkpoint.jsonl"), recursive=True):
        # Key by run-relative path, not stem: multi-depth layouts produce
        # L0/b8000.checkpoint.jsonl and L1/b8000.checkpoint.jsonl whose
        # identical stems would silently overwrite each other's rows.
        rel = Path(f).relative_to(d).as_posix()
        with open(f) as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                r = json.loads(line)
                ex = r.get("extra", {})
                out[f"{rel}::{r['instance_id']}"] = {
                    "status": ex.get("status"),
                    "selected_files": tuple(sorted(ex.get("selected_files") or [])),
                    "used_tokens": r.get("used_tokens"),
                    "file_recall": float(r.get("file_recall") or 0.0),
                    "file_precision": float(r.get("file_precision") or 0.0),
                }
    return out


def compare(a: dict[str, dict], b: dict[str, dict], tol: float = 1e-9) -> list[str]:
    problems: list[str] = []
    if set(a) != set(b):
        only_a = sorted(set(a) - set(b))[:5]
        only_b = sorted(set(b) - set(a))[:5]
        problems.append(f"instance sets differ: only_a={only_a} only_b={only_b}")
    for k in sorted(set(a) & set(b)):
        ra, rb = a[k], b[k]
        if ra["status"] != rb["status"]:
            problems.append(f"{k}: status {ra['status']} != {rb['status']}")
            continue
        if ra["selected_files"] != rb["selected_files"]:
            sa, sb = set(ra["selected_files"]), set(rb["selected_files"])
            problems.append(f"{k}: selected files differ (only_a={sorted(sa - sb)[:3]}, only_b={sorted(sb - sa)[:3]})")
        if ra["used_tokens"] != rb["used_tokens"]:
            problems.append(f"{k}: used_tokens {ra['used_tokens']} != {rb['used_tokens']}")
        for metric in ("file_recall", "file_precision"):
            if abs(ra[metric] - rb[metric]) > tol:
                problems.append(f"{k}: {metric} delta {abs(ra[metric] - rb[metric]):.3e} > {tol}")
    return problems


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--a", type=Path, required=True)
    p.add_argument("--b", type=Path, required=True)
    p.add_argument("--tol", type=float, default=1e-9)
    args = p.parse_args()
    ra, rb = load_run(args.a), load_run(args.b)
    if not ra or not rb:
        print(f"empty run dir: a={len(ra)} rows, b={len(rb)} rows")
        return 1
    problems = compare(ra, rb, args.tol)
    n = len(set(ra) & set(rb))
    if problems:
        print(f"EQUIVALENCE FAILED: {len(problems)} problems over {n} shared instances")
        for msg in problems[:40]:
            print(" ", msg)
        return 1
    print(f"EQUIVALENT: {n} instances, selected sets / tokens / metrics identical (tol={args.tol})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
