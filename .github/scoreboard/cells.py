#!/usr/bin/env python3
"""The 30 fixed cells of the bit-equivalence check, run by any binary over the pinned fixture checkout.

  cells.py run --bin PATH --fixture DIR --out DIR [--env K=V ...]
      one <name>.json per cell (wall-clock `latency` and the run record `provenance` removed, as the
      check removes them, keys sorted), <name>.provenance.json with what was removed, <name>.err for a
      cell that exited non-zero or printed nothing. Never stops at a failed cell.
  cells.py report --base DIR --head DIR [--sha SHA]
      per cell, what each side answered: for pack and locate the selected paths and their overlap,
      for impact and symbol whether any definition states a gap and how many rows it lists.
      There is no oracle for these cells, so none is judged right or wrong here.

Cells: 3 ranges x 4 scoring modes x {pack, locate}, one impact per range, and three symbols; the
ranges and symbols are the ones the pinned fixture was chosen for.
"""

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "hedge"))
import norm

RANGES = ("0d75e73a..99ff4abc", "384c4b86..3562b922", "d58814c9..0d75e73a")
MODES = ("ego", "ppr", "bm25", "rrf")
KINDS = ("pack", "locate")
SYMBOLS = ("run_git_z", "build_diff_context", "discover_related_files")
BUDGET = "8000"


def slug(text):
    return "".join(c if c.isalnum() else "_" for c in text)


def cells():
    for rng in RANGES:
        for mode in MODES:
            for kind in KINDS:
                yield f"{slug(rng)}__{mode}__{kind}", ["--diff", rng, "--scoring", mode, "--mode", kind, "--budget", BUDGET]
        yield f"{slug(rng)}__any__impact", ["--diff", rng, "--mode", "impact"]
    for symbol in SYMBOLS:
        yield f"symbol__{symbol}", ["--symbol", symbol]


def run(args):
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    env = {**os.environ, **dict(kv.split("=", 1) for kv in args.env)}
    failed = 0
    for name, cell_args in cells():
        proc = subprocess.run(
            [args.bin, args.fixture, *cell_args, "--format", "json", "--quiet"],
            env=env,
            capture_output=True,
        )
        if proc.returncode != 0 or not proc.stdout.strip():
            failed += 1
            (out / f"{name}.err").write_text(f"rc={proc.returncode}\n{proc.stderr.decode('utf-8', 'replace')}")
            print(f"cell failed: {name} rc={proc.returncode}", flush=True)
            continue
        try:
            doc = json.loads(proc.stdout)
        except ValueError as e:
            failed += 1
            (out / f"{name}.err").write_text(f"rc=0\nnot JSON: {e}\n")
            continue
        removed = {k: doc.pop(k) for k in ("latency", "provenance") if k in doc}
        (out / f"{name}.json").write_text(json.dumps(doc, indent=1, sort_keys=True))
        (out / f"{name}.provenance.json").write_text(json.dumps(removed, indent=1, sort_keys=True))
        print(f"  {name}", flush=True)
    print(f"{failed} of 30 cells failed")


def paths_of(doc):
    found = set()
    for key in ("fragments", "items"):
        for entry in doc.get(key) or []:
            if isinstance(entry, dict) and entry.get("path"):
                found.add(entry["path"])
    return found


def load(side, name):
    path = side / f"{name}.json"
    if not path.exists():
        err = side / f"{name}.err"
        return None, err.read_text().strip().splitlines()[0] if err.exists() else "missing"
    return json.loads(path.read_text()), ""


def report(args):
    base, head = Path(args.base), Path(args.head)
    rows = [
        "| cell | 1.20.0 | head | detail |",
        "|---|---|---|---|",
    ]
    overlaps = []
    for name, _ in cells():
        a, na = load(base, name)
        b, nb = load(head, name)
        if a is None or b is None:
            rows.append(f"| {name} | {na or 'ok'} | {nb or 'ok'} | no comparison |")
            continue
        if name.endswith(("__pack", "__locate")):
            pa, pb = paths_of(a), paths_of(b)
            union = pa | pb
            jac = len(pa & pb) / len(union) if union else 1.0
            overlaps.append(jac)
            rows.append(
                f"| {name} | {len(pa)} paths | {len(pb)} paths | overlap {len(pa & pb)} of {len(union)} ({jac:.0%}); "
                f"only 1.20.0 {len(pa - pb)}, only head {len(pb - pa)} |"
            )
        else:
            states = []
            for doc in (a, b):
                n = norm.normalize(doc)
                gapped = sum(bool(d["gaps"] or d["missed_in"]) for d in n["defs"].values())
                nrows = sum(len(d["rows"]) for d in n["defs"].values())
                states.append(f"{len(n['defs'])} defs, {nrows} rows, {'hedged on ' + str(gapped) if gapped else 'complete'}")
            rows.append(f"| {name} | {states[0]} | {states[1]} | answers have different schemas; not byte-comparable |")
    mean = sum(overlaps) / len(overlaps) if overlaps else 0.0
    title = "# The 30 bitcheck cells, 1.20.0 against the head" + (f" at {args.sha}" if args.sha else "")
    print(f"{title}\n\nmean path overlap over {len(overlaps)} pack/locate cells: {mean:.1%}\n")
    print("\n".join(rows))


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--bin", required=True)
    r.add_argument("--fixture", required=True)
    r.add_argument("--out", required=True)
    r.add_argument("--env", action="append", default=[])
    r.set_defaults(fn=run)
    p = sub.add_parser("report")
    p.add_argument("--base", required=True)
    p.add_argument("--head", required=True)
    p.add_argument("--sha", default="")
    p.set_defaults(fn=report)
    args = ap.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
