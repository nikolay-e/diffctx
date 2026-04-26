#!/usr/bin/env python3
from __future__ import annotations

import json
import statistics
from collections import defaultdict
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
RESULTS_DIR = REPO_ROOT / "results"
REPORT = RESULTS_DIR / "SWEEP_REPORT.md"

MODES = ["hybrid", "ppr", "ego", "bm25"]
BUDGETS = [-1, 0, 16000]
LIMIT = 9999

METRICS = [
    "file_recall",
    "nontrivial_file_recall",
    "line_recall",
    "line_recall_nontrivial",
]


def bootstrap_ci(vals, n_boot=2000, ci=0.95):
    import random

    if not vals:
        return (0.0, 0.0, 0.0)
    rng = random.Random(42)
    means = []
    n = len(vals)
    for _ in range(n_boot):
        sample = [vals[rng.randrange(n)] for _ in range(n)]
        means.append(sum(sample) / n)
    means.sort()
    lo = means[int((1 - ci) / 2 * n_boot)]
    hi = means[int((1 + ci) / 2 * n_boot)]
    return (sum(vals) / n, lo, hi)


def load_run(mode: str, budget: int) -> tuple[list[dict], dict] | None:
    path = RESULTS_DIR / f"cb_{mode}_n{LIMIT}_b{budget}.json"
    if not path.exists():
        return None
    data = json.loads(path.read_text())
    if isinstance(data, dict) and "results" in data:
        return list(data["results"]), dict(data.get("meta", {}))
    if isinstance(data, list):
        return data, {}
    return None


def fmt_metric(vals: list[float]) -> str:
    if not vals:
        return "n/a"
    mean, lo, hi = bootstrap_ci(vals)
    return f"{mean:.3f} [{lo:.3f}-{hi:.3f}]"


def per_config_summary() -> str:
    lines = ["## Per-config summary\n"]
    lines.append("| mode | budget | ok | clone_fail | other | mean elapsed (s) |" f" {' | '.join(METRICS)} |")
    lines.append("|" + "---|" * (5 + len(METRICS)))
    for mode in MODES:
        for budget in BUDGETS:
            run = load_run(mode, budget)
            if run is None:
                lines.append(f"| {mode} | {budget} | MISSING | - | - | - | " + " | ".join(["n/a"] * len(METRICS)) + " |")
                continue
            results, _ = run
            ok = [r for r in results if r.get("status") == "ok"]
            clone_fail = sum(1 for r in results if r.get("status") == "clone_fail")
            other = len(results) - len(ok) - clone_fail
            elapsed = [r["elapsed_s"] for r in ok if "elapsed_s" in r]
            mean_elapsed = f"{statistics.mean(elapsed):.1f}" if elapsed else "n/a"
            metric_cells = []
            for m in METRICS:
                vals = [r[m] for r in ok if m in r]
                metric_cells.append(fmt_metric(vals))
            lines.append(
                f"| {mode} | {budget} | {len(ok)} | {clone_fail} | {other} | "
                f"{mean_elapsed} | " + " | ".join(metric_cells) + " |"
            )
    return "\n".join(lines) + "\n"


def head_to_head() -> str:
    lines = ["\n## Head-to-head per budget (mean ± 95% CI on nontrivial_file_recall)\n"]
    for budget in BUDGETS:
        lines.append(f"\n### Budget = {budget}\n")
        lines.append("| mode | n | nontrivial_file_recall | line_recall_nontrivial |")
        lines.append("|---|---|---|---|")
        for mode in MODES:
            run = load_run(mode, budget)
            if run is None:
                lines.append(f"| {mode} | MISSING | n/a | n/a |")
                continue
            results, _ = run
            ok = [r for r in results if r.get("status") == "ok"]
            nfr = [r["nontrivial_file_recall"] for r in ok if "nontrivial_file_recall" in r]
            lrn = [r["line_recall_nontrivial"] for r in ok if "line_recall_nontrivial" in r]
            lines.append(f"| {mode} | {len(ok)} | {fmt_metric(nfr)} | {fmt_metric(lrn)} |")
    return "\n".join(lines) + "\n"


def budget_impact() -> str:
    lines = ["\n## Budget impact per mode (nontrivial_file_recall)\n"]
    lines.append("| mode | b=-1 (unlimited) | b=0 (default) | b=16000 |")
    lines.append("|---|---|---|---|")
    for mode in MODES:
        cells = [mode]
        for budget in BUDGETS:
            run = load_run(mode, budget)
            if run is None:
                cells.append("MISSING")
                continue
            results, _ = run
            ok = [r for r in results if r.get("status") == "ok"]
            nfr = [r["nontrivial_file_recall"] for r in ok if "nontrivial_file_recall" in r]
            cells.append(fmt_metric(nfr))
        lines.append("| " + " | ".join(cells) + " |")
    return "\n".join(lines) + "\n"


def per_language(mode: str, budget: int) -> dict[str, list[float]]:
    run = load_run(mode, budget)
    if run is None:
        return {}
    results, _ = run
    by_lang: dict[str, list[float]] = defaultdict(list)
    for r in results:
        if r.get("status") != "ok":
            continue
        lang = r.get("language", "?")
        if "nontrivial_file_recall" in r:
            by_lang[lang].append(r["nontrivial_file_recall"])
    return by_lang


def per_language_table() -> str:
    lines = ["\n## Per-language nontrivial_file_recall (best mode at b=16000)\n"]
    target_budget = 16000
    all_langs = set()
    by_mode: dict[str, dict[str, list[float]]] = {}
    for mode in MODES:
        by_mode[mode] = per_language(mode, target_budget)
        all_langs.update(by_mode[mode].keys())
    if not all_langs:
        return "_no data at b=16000_\n"
    lines.append("| language | " + " | ".join(MODES) + " |")
    lines.append("|" + "---|" * (1 + len(MODES)))
    for lang in sorted(all_langs):
        cells = [lang]
        for mode in MODES:
            vals = by_mode[mode].get(lang, [])
            if vals:
                cells.append(f"{statistics.mean(vals):.3f} (n={len(vals)})")
            else:
                cells.append("n/a")
        lines.append("| " + " | ".join(cells) + " |")
    return "\n".join(lines) + "\n"


def raw_files_index() -> str:
    lines = ["\n## Raw result files\n"]
    for mode in MODES:
        for budget in BUDGETS:
            path = RESULTS_DIR / f"cb_{mode}_n{LIMIT}_b{budget}.json"
            status = "✓" if path.exists() else "✗"
            size = f"{path.stat().st_size // 1024}KB" if path.exists() else "-"
            lines.append(f"- {status} `cb_{mode}_n{LIMIT}_b{budget}.json` ({size})")
    return "\n".join(lines) + "\n"


def main() -> None:
    from datetime import datetime, timezone

    out: list[str] = []
    out.append("# Diffctx Sweep Report\n")
    out.append(f"Generated: {datetime.now(timezone.utc).isoformat()}\n")
    out.append("Dataset: ContextBench full, nontrivial only (~672 instances per run)\n")
    out.append("Configs: 4 scoring modes x 3 budgets = 12 runs\n")
    out.append(per_config_summary())
    out.append(head_to_head())
    out.append(budget_impact())
    out.append(per_language_table())
    out.append(raw_files_index())
    REPORT.write_text("\n".join(out))
    print(f"Report written: {REPORT}")


if __name__ == "__main__":
    main()
