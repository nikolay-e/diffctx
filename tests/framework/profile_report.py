"""Where a test run spent its time inside diffctx, read off the lines the engine
appends to `DIFFCTX_PROFILE` (one per run). Stdlib only: the Rust suite's script
runs it with whatever python3 the runner has."""

from __future__ import annotations

import json
import sys
from collections import defaultdict
from pathlib import Path

UNATTRIBUTED = "(unattributed)"


def read_runs(path: Path) -> list[dict]:
    runs = []
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return runs
    for line in lines:
        try:
            run = json.loads(line)
        except ValueError:
            continue
        if isinstance(run, dict) and isinstance(run.get("total_ms"), (int, float)):
            runs.append(run)
    return runs


def phases_of(run: dict) -> dict[str, float]:
    phases = {k: float(v) for k, v in (run.get("phases") or {}).items()}
    rest = float(run["total_ms"]) - sum(phases.values())
    if rest > 0.5:
        phases[UNATTRIBUTED] = rest
    return phases


def dominant(run: dict) -> str:
    phases = phases_of(run)
    if not phases:
        return "-"
    name, ms = max(phases.items(), key=lambda kv: kv[1])
    return f"{name} {100 * ms / max(float(run['total_ms']), 1e-9):.0f}%"


def _seconds(ms: float) -> str:
    return f"{ms / 1000:.1f}s"


def _percentile(values: list[float], q: float) -> float:
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(q * len(ordered)))]


def _table(header: list[str], rows: list[list[str]], left: set[int]) -> list[str]:
    widths = [max(len(h), *(len(r[i]) for r in rows)) if rows else len(h) for i, h in enumerate(header)]

    def fmt(cells: list[str]) -> str:
        out = [c.ljust(w) if i in left else c.rjust(w) for i, (c, w) in enumerate(zip(cells, widths, strict=True))]
        return "  ".join(out).rstrip()

    return [fmt(header), *(fmt(r) for r in rows)]


def summarize(runs: list[dict], top: int = 10, source: str = "") -> str:
    total = sum(float(r["total_ms"]) for r in runs)
    head = f"{len(runs)} runs, {_seconds(total)} inside diffctx" if runs else "no runs recorded"
    lines = [f"diffctx profile: {head}{f' ({source})' if source else ''}"]
    if not runs:
        return lines[0]

    by_phase: dict[str, list[float]] = defaultdict(list)
    for r in runs:
        for name, ms in phases_of(r).items():
            by_phase[name].append(ms)
    phase_rows = [
        [name, _seconds(sum(ms)), f"{100 * sum(ms) / total:.1f}%", str(len(ms)), f"{max(ms):.0f}ms"]
        for name, ms in sorted(by_phase.items(), key=lambda kv: -sum(kv[1]))
    ]
    lines += ["", "Where the time went, every run together:"]
    lines += ["  " + line for line in _table(["phase", "total", "share", "runs", "slowest"], phase_rows, {0})]

    by_mode: dict[str, list[float]] = defaultdict(list)
    for r in runs:
        by_mode[str(r.get("mode", "?"))].append(float(r["total_ms"]))
    mode_rows = [
        [mode, str(len(ms)), _seconds(sum(ms)), f"{sum(ms) / len(ms):.0f}ms", f"{_percentile(ms, 0.95):.0f}ms"]
        for mode, ms in sorted(by_mode.items(), key=lambda kv: -sum(kv[1]))
    ]
    lines += ["", "By mode:"]
    lines += ["  " + line for line in _table(["mode", "runs", "total", "mean", "p95"], mode_rows, {0})]

    by_tag: dict[str, list[dict]] = defaultdict(list)
    for r in runs:
        by_tag[r.get("tag") or "(untagged)"].append(r)
    tag_rows = []
    for tag, rs in sorted(by_tag.items(), key=lambda kv: -sum(float(r["total_ms"]) for r in kv[1]))[:top]:
        slowest = max(rs, key=lambda r: float(r["total_ms"]))
        tag_rows.append([_seconds(sum(float(r["total_ms"]) for r in rs)), str(len(rs)), dominant(slowest), tag])
    lines += ["", f"Tests that ran diffctx longest (top {len(tag_rows)} of {len(by_tag)}):"]
    lines += ["  " + line for line in _table(["total", "runs", "slowest run's main phase", "test"], tag_rows, {2, 3})]

    run_rows = []
    for r in sorted(runs, key=lambda r: -float(r["total_ms"]))[:top]:
        counts = r.get("counts") or {}
        size = "/".join(str(counts.get(k, "-")) for k in ("candidate_files", "fragments", "edges"))
        rss = r.get("peak_rss_mb")
        run_rows.append(
            [
                f"{float(r['total_ms']):.0f}ms",
                str(r.get("mode", "?")),
                dominant(r),
                size,
                f"{rss:.0f}MB" if isinstance(rss, (int, float)) else "-",
                r.get("tag") or "(untagged)",
            ]
        )
    lines += ["", f"Slowest single runs (top {len(run_rows)}):"]
    lines += [
        "  " + line
        for line in _table(["time", "mode", "main phase", "files/frags/edges", "peak", "test"], run_rows, {1, 2, 3, 5})
    ]
    return "\n".join(lines)


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(f"usage: {Path(argv[0]).name} PROFILE.jsonl", file=sys.stderr)
        return 2
    path = Path(argv[1])
    print(summarize(read_runs(path), source=str(path)))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
