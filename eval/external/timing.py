"""Per-instance engine wall time over external runs: a base engine against another.

Reads the shard outputs the external mode uploads (`ext-shard-*` artifacts) and
compares, per dataset, the engine's own time on each instance: quantiles for
each side, and for the instances both sides ran, the ratio head / base.

    python -m eval external-timing --base 40734249322 --head <run id> [--out timing.json]
    python -m eval external-timing --base DIR --head DIR

A side is a run id (downloaded with `gh run download`) or a directory holding
the shard outputs. What counts as engine time differs by dataset and is named in
the output: the call to `locate` for ARB (`engine_s`, clone excluded), the
`diffctx` call for ContextBench and SWE-rebench. Hosted runners vary from run to
run, so compare two runs of the same engine first to see the noise a ratio has
to clear.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import tempfile
from pathlib import Path
from typing import Any

QUANTILES = (0.5, 0.9, 0.95, 0.99)


def quantile(sorted_values: list[float], q: float) -> float:
    if not sorted_values:
        return 0.0
    position = q * (len(sorted_values) - 1)
    low = int(position)
    high = min(low + 1, len(sorted_values) - 1)
    return sorted_values[low] + (sorted_values[high] - sorted_values[low]) * (position - low)


def describe(times: dict[str, float]) -> dict[str, Any]:
    values = sorted(times.values())
    out: dict[str, Any] = {"n": len(values), "total_s": round(sum(values), 3)}
    if values:
        out["mean_s"] = round(sum(values) / len(values), 3)
        out.update({f"p{round(q * 100)}_s": round(quantile(values, q), 3) for q in QUANTILES})
        out["max_s"] = round(values[-1], 3)
    return out


def _arb(path: Path) -> dict[str, float]:
    rows = (json.loads(line) for line in path.read_text().splitlines() if line.strip())
    return {r["sample_id"]: float(r["engine_s"]) for r in rows if r.get("status") == "ok" and r.get("engine_s") is not None}


def _contextbench(path: Path) -> dict[str, float]:
    rows = json.loads(path.read_text())
    return {r["instance_id"]: float(r["elapsed_seconds"]) for r in rows if (r.get("extra") or {}).get("status") == "ok"}


def _rows_json(path: Path) -> tuple[str, dict[str, float]] | None:
    rows = json.loads(path.read_text())
    if rows and all("direction" in r and "elapsed_s" in r for r in rows):
        return "swerebench_infra", {
            f"{r['instance_id']}|{r['direction']}": float(r["elapsed_s"]) for r in rows if r.get("status") == "ok"
        }
    return None  # cfgnet500 rows carry no per-instance time


def collect(root: Path) -> dict[str, dict[str, float]]:
    """Dataset name -> instance key -> engine seconds, from every shard output under `root`."""
    found: dict[str, dict[str, float]] = {}
    for path in sorted(root.rglob("rankings.jsonl")):
        found.setdefault("arb_edit2ripple", {}).update(_arb(path))
    for path in sorted(root.rglob("contextbench_verified.json")):
        found.setdefault("contextbench_nontrivial", {}).update(_contextbench(path))
    for path in sorted(root.rglob("rows.json")):
        parsed = _rows_json(path)
        if parsed:
            found.setdefault(parsed[0], {}).update(parsed[1])
    return found


SOURCE = {
    "arb_edit2ripple": "engine_s: the locate call, clone and patch excluded",
    "contextbench_nontrivial": "elapsed_seconds: the diffctx call of the sweep's eval function",
    "swerebench_infra": "elapsed_s: the diffctx call, one row per instance and direction",
}


def compare(base: dict[str, dict[str, float]], head: dict[str, dict[str, float]]) -> dict[str, Any]:
    report: dict[str, Any] = {}
    for dataset in sorted(set(base) | set(head)):
        b, h = base.get(dataset, {}), head.get(dataset, {})
        shared = sorted(set(b) & set(h))
        entry: dict[str, Any] = {"time_is": SOURCE.get(dataset, ""), "base": describe(b), "head": describe(h)}
        ratios = sorted(h[k] / b[k] for k in shared if b[k] > 0)
        if ratios:
            entry["paired"] = {
                "n": len(ratios),
                "median_ratio": round(quantile(ratios, 0.5), 3),
                "p90_ratio": round(quantile(ratios, 0.9), 3),
                "total_ratio": round(sum(h[k] for k in shared) / sum(b[k] for k in shared), 3),
                "slower_than_1_5x": sum(1 for r in ratios if r > 1.5),
            }
        report[dataset] = entry
    return report


def render(report: dict[str, Any]) -> str:
    lines = [
        "| dataset | side | n | mean s | p50 | p90 | p95 | p99 | max | total s |",
        "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for dataset, entry in report.items():
        for side in ("base", "head"):
            d = entry[side]
            if not d["n"]:
                lines.append(f"| {dataset} | {side} | 0 |  |  |  |  |  |  |  |")
                continue
            cells = [d["mean_s"], d["p50_s"], d["p90_s"], d["p95_s"], d["p99_s"], d["max_s"], d["total_s"]]
            lines.append(f"| {dataset} | {side} | {d['n']} | " + " | ".join(f"{c:g}" for c in cells) + " |")
    lines += ["", "| dataset | paired n | median head/base | p90 | total | slower than 1.5x |", "|---|---:|---:|---:|---:|---:|"]
    for dataset, entry in report.items():
        p = entry.get("paired")
        if p:
            lines.append(
                f"| {dataset} | {p['n']} | {p['median_ratio']} | {p['p90_ratio']} | {p['total_ratio']} | {p['slower_than_1_5x']} |"
            )
    return "\n".join(lines) + "\n"


def resolve(spec: str, scratch: Path, side: str) -> Path:
    if not spec.isdigit():
        return Path(spec)
    target = scratch / side / spec
    subprocess.run(
        ["gh", "run", "download", spec, "--pattern", "ext-shard-*", "-D", str(target)], check=True, stdin=subprocess.DEVNULL
    )
    return target


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--base", required=True, help="run id or directory of shard outputs")
    parser.add_argument("--head", required=True, help="run id or directory of shard outputs")
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as scratch:
        report = compare(collect(resolve(args.base, Path(scratch), "base")), collect(resolve(args.head, Path(scratch), "head")))
    if args.out:
        args.out.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(render(report))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
