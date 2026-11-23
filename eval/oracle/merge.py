"""Join the parts of one round, each measured by its own job, into the one run directory `report.py` reads."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from measure import RESULT_FILES, RUN_FILE


def merge(parts: list[Path], out: Path) -> dict:
    """Concatenate each result file of every part; every part must come from one engine at one budget."""
    records = [json.loads((part / RUN_FILE).read_text(encoding="utf-8")) for part in parts if (part / RUN_FILE).exists()]
    if not records:
        raise SystemExit("no part carries a run record: nothing was measured")
    different = [
        r
        for r in records
        if (r.get("engine"), r.get("budget"), r.get("fresh"))
        != (records[0].get("engine"), records[0].get("budget"), records[0].get("fresh"))
    ]
    if different:
        raise SystemExit(f"the parts were not measured by one engine at one budget: {different[0]} against {records[0]}")
    out.mkdir(parents=True, exist_ok=True)
    (out / RUN_FILE).write_text(json.dumps(records[0], sort_keys=True) + "\n", encoding="utf-8")
    for name in RESULT_FILES:
        with (out / name).open("w", encoding="utf-8") as target:
            for part in parts:
                if (part / name).exists():
                    text = (part / name).read_text(encoding="utf-8")
                    target.write(text if text.endswith("\n") or not text else text + "\n")
    return records[0]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="merge", description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("parts", nargs="+", type=Path)
    options = parser.parse_args(argv)
    run = merge(options.parts, options.out)
    print(
        f"merged {len(options.parts)} parts into {options.out} ({run.get('engine')}, budget {run.get('budget')})", file=sys.stderr
    )
    return 0
