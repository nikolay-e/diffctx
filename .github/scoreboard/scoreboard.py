#!/usr/bin/env python3
"""scoreboard.md from the probe's artifacts and reports: one summary table, then every section whole.

  scoreboard.py --art DIR --reports DIR --sha PRODUCT_SHA --probe PROBE_SHA --run URL --needs needs.json --out scoreboard.md

Every row of the summary names the sha. A number whose input is missing reads `not available` with the
job's result, never zero.
"""

import argparse
import csv
import json
import re
from datetime import datetime
from pathlib import Path

NA = "not available"


def read(path):
    p = Path(path)
    return p.read_text(errors="replace") if p.exists() else None


def first(pattern, text, group=1, flags=re.M):
    m = re.search(pattern, text or "", flags)
    return m.group(group) if m else None


def section(title, body, level="##"):
    return f"{level} {title}\n\n{body.strip() if body else NA}\n"


def code_blocks_top(profile, n):
    """{cell: {tag: first n lines}} of profile.py's inclusive listings."""
    out = {}
    if not profile:
        return out
    for cell_block in re.split(r"^### ", profile, flags=re.M)[1:]:
        cell = cell_block.split(":", 1)[0]
        for tag in ("1.20.0", "head"):
            m = re.search(rf"^{re.escape(tag)}:\n```\n(.*?)\n```", cell_block, re.S | re.M)
            if m:
                out.setdefault(cell, {})[tag] = m.group(1).splitlines()[:n]
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--art", required=True)
    ap.add_argument("--reports", required=True)
    ap.add_argument("--sha", required=True)
    ap.add_argument("--probe", required=True)
    ap.add_argument("--run", default="")
    ap.add_argument("--needs", required=True)
    ap.add_argument("--jobs", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    art, rep = Path(a.art), Path(a.reports)
    needs = json.loads(Path(a.needs).read_text())
    result = lambda *jobs: ", ".join(f"{j}: {needs.get(j, {}).get('result', 'unknown')}" for j in jobs)  # noqa: E731
    rows = []  # (measure, value, jobs)

    def row(measure, value, *jobs):
        rows.append((measure, value if value else NA, result(*jobs)))

    # HEDGE
    classes = rep / "hedge" / "classes.tsv"
    if classes.exists():
        data = list(csv.DictReader(classes.open(), delimiter="\t"))
        n = len(data)
        share = lambda key: sum(r[key] == "confident-right" for r in data) / n if n else 0  # noqa: E731
        reg = [r for r in data if r["regression"] == "True"]
        rw = [r for r in data if r["right_to_wrong"] == "True"]
        row(
            "HEDGE: confident-and-right share, 1.20.0 -> head",
            f"{share('base'):.1%} -> {share('head'):.1%} over {n} corpus cases",
            "hedge",
        )
        row(
            "HEDGE: regressions (1.20.0 confident and right, head not)",
            f"{len(reg)} ({sum(r['head'].startswith('hedged') for r in reg)} hedged, {sum(r['head'] == 'confident-wrong' for r in reg)} wrong); "
            f"right to wrong {len(rw)}",
            "hedge",
        )
    else:
        row("HEDGE: confident-and-right share and regressions", None, "hedge")
    transitions = rep / "hedge" / "transitions.jsonl"
    if transitions.exists():
        t = [json.loads(line) for line in transitions.read_text().splitlines()]
        row(
            "HEDGE: compare.py REGRESSION transitions",
            f"{sum(x['label'] == 'REGRESSION' for x in t)} of {len(t)} transitions",
            "hedge",
        )
    cells = read(art / "hedge-cells" / "cells.md")
    row(
        "HEDGE cells: mean pack/locate path overlap with 1.20.0",
        first(r"mean path overlap over (\d+ pack/locate cells: [\d.]+%)", cells),
        "hedge-cells",
    )

    # vs120
    perf = read(art / "vs120" / "perf.md")
    for cell_name in ("impact-narrow", "locate-ppr-narrow", "pack-ego-wide", "symbol"):
        r = first(rf"^\| {cell_name} \|[^|]*\|[^|]*\| ([\d.]+x \([+-][\d.]+%\)) \|", perf)
        row(f"vs120 instructions, head/1.20.0, {cell_name}", r, "vs120")
    row("vs120 control (1.20.0 run twice), worst spread", first(r"against itself: worst ([\d.]+%)", perf), "vs120")
    gits = read(art / "vs120" / "gitcount.md")
    for cell_name in ("impact-narrow", "locate-ppr-narrow", "pack-ego-wide", "symbol"):
        m = re.search(rf"^\| {cell_name} \| (\d+) \| (\d+) \|", gits or "", re.M)
        row(f"vs120 git processes, 1.20.0 / head, {cell_name}", f"{m.group(1)} / {m.group(2)}" if m else None, "vs120")

    # O15
    o15 = read(art / "o15-perf" / "perf.md")
    for cell_name in ("locate-ppr-narrow", "locate-ego-wide", "pack-ego-wide", "pack-ppr-narrow"):
        m = re.search(rf"^\| {cell_name} \|[^|]*\|[^|]*\| ([\d.]+x \([+-][\d.]+%\)) \|[^|]*\| (\w+) \|", o15 or "", re.M)
        row(f"O15 instructions all/none, {cell_name}", f"{m.group(1)}, answer {m.group(2)}" if m else None, "o15-perf")
    row("O15 control (none run twice), worst spread", first(r"none against itself: worst ([\d.]+%)", o15), "o15-perf")
    o15c = read(rep / "o15-corpus.md")
    m = re.search(r"mean score over the (\d+) cases scored in both \| ([\d.]+%) \| ([\d.]+%)", o15c or "")
    row(
        "O15 corpus mean selection score, none -> all",
        f"{m.group(2)} -> {m.group(3)} over {m.group(1)} cases" if m else None,
        "o15-corpus",
    )
    m = re.search(r"mean forbidden_rate[^|]*\| ([\d.]+%) \| ([\d.]+%)", o15c or "")
    row("O15 corpus mean forbidden_rate, none -> all", f"{m.group(1)} -> {m.group(2)}" if m else None, "o15-corpus")
    m = re.search(r"cases scoring higher with all: (\d+); lower: (\d+)", o15c or "")
    row("O15 corpus cases higher / lower with all", f"{m.group(1)} / {m.group(2)}" if m else None, "o15-corpus")

    # coverage
    cov = read(rep / "coverage" / "coverage.md")
    m = re.search(r"^\| \*\*all\*\* \| (\d+) \| (\d+) \| (\d+) \| (\d+) \| (\d+) \|", cov or "", re.M)
    row(
        "Coverage: product lines, Rust tests / pytest / either, neither",
        (
            f"{m.group(1)} lines: {m.group(2)} / {m.group(3)} / {int(m.group(1)) - int(m.group(5))}, neither {m.group(5)}"
            if m
            else None
        ),
        "coverage-rust",
        "coverage-python",
    )

    # determinism
    det = read(art / "determinism" / "determinism.md")
    row(
        "Determinism: cells differing across RAYON_NUM_THREADS runs",
        first(r"differs from run 1's[^:]*: (\d+)", det),
        "determinism",
    )

    # identity
    ident = read(art / "identity" / "summary.md")
    row("identity_corpus: tests passed / failed, violations, without an account", first(r"^totals: (.*)$", ident), "identity")

    lines = [
        f"# Scoreboard at {a.sha}",
        "",
        f"Product sha `{a.sha}`; probe commit `{a.probe}` (the product sha plus the probe's own files); run {a.run or '(local)'}.",
        "Every row below was measured on that sha.",
        "",
        "| measure | result | sha | job result |",
        "|---|---|---|---|",
    ]
    lines += [f"| {m} | {v} | `{a.sha[:12]}` | {j} |" for m, v, j in rows]
    lines += ["", "## Jobs", "", "| job | conclusion | wall time |", "|---|---|---|"]
    for job in json.loads(Path(a.jobs).read_text()).get("jobs", []):
        start, end = job.get("started_at"), job.get("completed_at")
        wall = NA
        if start and end:
            seconds = int(
                (
                    datetime.fromisoformat(end.replace("Z", "+00:00")) - datetime.fromisoformat(start.replace("Z", "+00:00"))
                ).total_seconds()
            )
            wall = f"{seconds // 60} min {seconds % 60} s"
        lines.append(f"| {job['name']} | {job.get('conclusion') or job.get('status')} | {wall} |")
    lines.append("")

    body = [
        section("Build", read(art / "build-manifest" / "manifest.txt")),
        section("HEDGE: confident and right against 1.20.0", read(rep / "hedge" / "classes.md")),
        section("HEDGE: transitions (compare.py summary)", read(rep / "hedge" / "summary.md")),
        section("HEDGE: the 30 bitcheck cells", cells),
        section("vs120: instructions", perf),
        section("vs120: git processes", gits),
    ]
    profile = read(art / "vs120" / "profile.md")
    top = code_blocks_top(profile, 10)
    text = []
    for cell_name in ("impact-narrow", "locate-ppr-narrow", "symbol", "pack-ego-wide"):
        for tag in ("1.20.0", "head"):
            block = top.get(cell_name, {}).get(tag)
            if block:
                text.append(f"{cell_name}, {tag}, top 10 by inclusive instructions:\n```\n" + "\n".join(block) + "\n```")
    body.append(section("vs120: the ten most expensive functions per mode (top 40 in vs120/profile.md)", "\n\n".join(text)))
    body += [
        section("O15: instructions, DIFFCTX_CELL_EDGES none against all", o15),
        section("O15: selection scores on the corpus", o15c),
        section("Coverage", cov),
        section("Determinism", read(art / "determinism" / "determinism.md")),
        section("identity_corpus", ident),
    ]
    Path(a.out).write_text("\n".join(lines) + "\n" + "\n".join(body))
    print("\n".join(lines))


if __name__ == "__main__":
    main()
