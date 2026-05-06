"""Per-dataset descriptive table — paper-ready Section "Datasets".

Iterates each `BenchmarkAdapter`, materializes every instance, and emits a
markdown table of |gold|, |changed_files|, ratio, language mix, single/multi-file
breakdown, gold-quality sanity checks. Independent of any sweep run.

CLI:
    python -m benchmarks.dataset_describe              # default test adapters
    python -m benchmarks.dataset_describe --include-calibration
    python -m benchmarks.dataset_describe --out report.md
"""

from __future__ import annotations

import argparse
import statistics
import sys
from collections.abc import Sequence
from pathlib import Path

from benchmarks.adapters.base import BenchmarkAdapter, BenchmarkInstance
from benchmarks.build_splits import default_calibration_pool_adapters, default_test_adapters
from benchmarks.cell_metrics import _percentile
from benchmarks.common import patch_size_metrics


def _percentiles(values: Sequence[float]) -> dict[str, float]:
    if not values:
        return {"n": 0, "mean": 0.0, "p5": 0.0, "p50": 0.0, "p95": 0.0, "max": 0.0}
    vs = sorted(float(v) for v in values)
    return {
        "n": len(vs),
        "mean": statistics.fmean(vs),
        "p5": _percentile(vs, 0.05),
        "p50": _percentile(vs, 0.5),
        "p95": _percentile(vs, 0.95),
        "max": vs[-1],
    }


def _gold_lines(instance: BenchmarkInstance) -> int:
    if instance.gold_fragments is None:
        return 0
    total = 0
    for g in instance.gold_fragments:
        if g.is_whole_file():
            continue
        if g.start_line is None or g.end_line is None:
            continue
        total += g.end_line - g.start_line + 1
    return total


def describe_adapter(adapter: BenchmarkAdapter) -> dict:
    n_gold_files: list[float] = []
    n_changed_files: list[float] = []
    n_hunks: list[float] = []
    diff_size_lines: list[float] = []
    n_gold_fragments: list[float] = []
    n_gold_lines: list[float] = []
    ratios: list[float] = []
    languages: dict[str, int] = {}
    single_file_gold = 0
    multi_file_gold = 0
    whole_file_gold = 0
    fragment_level_gold = 0
    zero_gold = 0
    duplicate_ids: dict[str, int] = {}
    seen_ids: set[str] = set()
    n_instances = 0

    for inst in adapter.load():
        n_instances += 1
        if inst.instance_id in seen_ids:
            duplicate_ids[inst.instance_id] = duplicate_ids.get(inst.instance_id, 1) + 1
        seen_ids.add(inst.instance_id)
        n_g = len(inst.gold_files)
        if n_g == 0:
            zero_gold += 1
        elif n_g == 1:
            single_file_gold += 1
        else:
            multi_file_gold += 1
        languages[inst.language] = languages.get(inst.language, 0) + 1
        ps = patch_size_metrics(inst.gold_patch)
        n_changed = ps["n_changed_files"]
        n_gold_files.append(n_g)
        n_changed_files.append(n_changed)
        n_hunks.append(ps["n_hunks"])
        diff_size_lines.append(ps["diff_size_lines"])
        if n_changed > 0:
            ratios.append(n_g / n_changed)
        if inst.gold_fragments:
            n_gold_fragments.append(len(inst.gold_fragments))
            n_gold_lines.append(_gold_lines(inst))
            n_whole = sum(1 for g in inst.gold_fragments if g.is_whole_file())
            if n_whole == len(inst.gold_fragments):
                whole_file_gold += 1
            else:
                fragment_level_gold += 1

    return {
        "name": adapter.name,
        "n_instances": n_instances,
        "n_gold_files": _percentiles(n_gold_files),
        "n_changed_files": _percentiles(n_changed_files),
        "n_hunks": _percentiles(n_hunks),
        "diff_size_lines": _percentiles(diff_size_lines),
        "n_gold_fragments": _percentiles(n_gold_fragments) if n_gold_fragments else None,
        "n_gold_lines": _percentiles(n_gold_lines) if n_gold_lines else None,
        "gold_to_changed_ratio": _percentiles(ratios),
        "single_file_pct": 100.0 * single_file_gold / n_instances if n_instances else 0.0,
        "multi_file_pct": 100.0 * multi_file_gold / n_instances if n_instances else 0.0,
        "zero_gold_pct": 100.0 * zero_gold / n_instances if n_instances else 0.0,
        "whole_file_gold_pct": 100.0 * whole_file_gold / n_instances if n_instances else 0.0,
        "fragment_level_gold_pct": 100.0 * fragment_level_gold / n_instances if n_instances else 0.0,
        "languages": dict(sorted(languages.items(), key=lambda x: -x[1])),
        "duplicate_instance_ids": duplicate_ids,
    }


def render_report(reports: list[dict]) -> str:
    if not reports:
        return "(no adapters)\n"
    out: list[str] = ["# Dataset characterization\n"]
    out.append("| metric | " + " | ".join(r["name"] for r in reports) + " |")
    out.append("|---|" + "---|" * len(reports))

    def cell(getter) -> str:
        return " | ".join(getter(r) for r in reports)

    def fmt(v: object, ndigits: int = 2) -> str:
        if isinstance(v, (int, float)):
            return f"{v:.{ndigits}f}" if isinstance(v, float) else str(v)
        return str(v)

    out.append("| **n_instances** | " + cell(lambda r: str(r["n_instances"])) + " |")
    out.append("| mean(\\|gold_files\\|) | " + cell(lambda r: fmt(r["n_gold_files"]["mean"])) + " |")
    out.append(
        "| P5/P50/P95(\\|gold_files\\|) | "
        + cell(lambda r: f"{fmt(r['n_gold_files']['p5'])}/{fmt(r['n_gold_files']['p50'])}/{fmt(r['n_gold_files']['p95'])}")
        + " |"
    )
    out.append("| max(\\|gold_files\\|) | " + cell(lambda r: fmt(r["n_gold_files"]["max"])) + " |")
    out.append("| mean(\\|changed_files\\|) | " + cell(lambda r: fmt(r["n_changed_files"]["mean"])) + " |")
    out.append("| mean(diff_size_lines) | " + cell(lambda r: fmt(r["diff_size_lines"]["mean"], 1)) + " |")
    out.append("| mean(\\|gold\\|/\\|changed\\|) | " + cell(lambda r: fmt(r["gold_to_changed_ratio"]["mean"], 3)) + " |")
    out.append("| P95(\\|gold\\|/\\|changed\\|) | " + cell(lambda r: fmt(r["gold_to_changed_ratio"]["p95"], 3)) + " |")
    out.append("| **% single-file gold** | " + cell(lambda r: f"{r['single_file_pct']:.1f}%") + " |")
    out.append("| **% multi-file gold** | " + cell(lambda r: f"{r['multi_file_pct']:.1f}%") + " |")
    out.append("| % zero-gold (sanity) | " + cell(lambda r: f"{r['zero_gold_pct']:.1f}%") + " |")
    out.append("| % whole-file gold | " + cell(lambda r: f"{r['whole_file_gold_pct']:.1f}%") + " |")
    out.append("| % fragment-level gold | " + cell(lambda r: f"{r['fragment_level_gold_pct']:.1f}%") + " |")
    out.append(
        "| mean(\\|gold_fragments\\|) | "
        + cell(lambda r: fmt(r["n_gold_fragments"]["mean"]) if r["n_gold_fragments"] else "—")
        + " |"
    )
    out.append("| mean(gold_lines) | " + cell(lambda r: fmt(r["n_gold_lines"]["mean"], 1) if r["n_gold_lines"] else "—") + " |")

    out.append("\n## Language mix per dataset\n")
    all_langs: list[str] = []
    for r in reports:
        for lang in r["languages"]:
            if lang not in all_langs:
                all_langs.append(lang)
    out.append("| language | " + " | ".join(r["name"] for r in reports) + " |")
    out.append("|---|" + "---|" * len(reports))
    for lang in all_langs:
        cells = [str(r["languages"].get(lang, 0)) for r in reports]
        out.append(f"| {lang} | " + " | ".join(cells) + " |")

    out.append("\n## Sanity flags\n")
    for r in reports:
        flags = []
        if r["zero_gold_pct"] > 0:
            flags.append(f"{r['zero_gold_pct']:.1f}% instances with |gold|=0")
        if r["duplicate_instance_ids"]:
            flags.append(f"{len(r['duplicate_instance_ids'])} duplicate instance_ids")
        out.append(f"- **{r['name']}**: " + ("; ".join(flags) if flags else "no issues"))

    return "\n".join(out) + "\n"


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--include-calibration", action="store_true", help="also describe calibration adapters")
    p.add_argument("--out", type=Path, default=None, help="output path (default: stdout)")
    args = p.parse_args()

    adapters: list[BenchmarkAdapter] = list(default_test_adapters())
    if args.include_calibration:
        adapters.extend(default_calibration_pool_adapters())

    reports: list[dict] = []
    for adapter in adapters:
        sys.stderr.write(f"Loading {adapter.name}...\n")
        try:
            reports.append(describe_adapter(adapter))
        except Exception as e:
            sys.stderr.write(f"  FAILED: {e}\n")

    text = render_report(reports)
    if args.out:
        args.out.write_text(text)
    else:
        sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
