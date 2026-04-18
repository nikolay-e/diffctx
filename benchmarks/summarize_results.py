#!/usr/bin/env python3
from __future__ import annotations

import sys
from pathlib import Path

from common import load_results


def _print_txt_section(txt: Path, prefix: str, title: str, markers: tuple[str, ...]) -> None:
    mode = txt.stem.replace(prefix, "")
    print(f"### {title} ({mode})\n```")
    for line in txt.read_text().splitlines():
        if line.startswith(markers):
            print(line)
    print("```\n")


def main() -> None:
    results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("results")

    print("## Benchmark Results\n")

    for txt in sorted(results_dir.glob("cb_*.txt")):
        _print_txt_section(txt, "cb_", "ContextBench", ("Avg ", "Total:"))

    for txt in sorted(results_dir.glob("loo_*.txt")):
        _print_txt_section(txt, "loo_", "LOO", ("Total LOO", "Found"))

    for jf in sorted(results_dir.glob("loo_*.json")):
        mode = jf.stem.replace("loo_", "")
        data = load_results(jf)
        found = sum(1 for r in data if r["found"])
        total = len(data)
        pct = 100 * found / total if total else 0
        print(f"**LOO {mode} recall: {found}/{total} ({pct:.1f}%)**\n")


if __name__ == "__main__":
    main()
