"""Why a gold file is missing: never surfaced, or surfaced and not selected.

Those are different failures with different fixes, and the selected set alone
cannot tell them apart — a file absent from the output looks identical whether
discovery never proposed it or the greedy ranked it out. #130 is about the first
and #65/#123 about the second, so pooling them makes both undiagnosable.

Reads the env-gated provenance dump (`DIFFCTX_PROVENANCE_DUMP=<path>`), which
carries one JSONL row per scored candidate with `discovery_source` — the
strategy that first surfaced the path, or null for a changed file, which is a
seed rather than a discovery.

A gold file that appears in no row at all was never in the universe. One that
appears with `selected: false` was surfaced and outranked. The split is the
whole point of the module.

CLI:
    python -m eval.analysis.discovery_attribution --dump DUMP.jsonl --gold a.py --gold b.py
    python -m eval.analysis.discovery_attribution --dump-dir DIR --gold-map gold.json
"""

from __future__ import annotations

import argparse
import json
from collections import Counter
from pathlib import Path


def load_dump(path: Path) -> list[dict]:
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def attribute(rows: list[dict], gold: set[str]) -> dict:
    """Split `gold` into selected / surfaced-not-selected / never-surfaced.

    Paths are matched by suffix: the dump carries absolute paths while gold
    labels are repo-relative, and exact equality would put every gold file in
    the never-surfaced bucket — the failure mode that would make this module
    confidently wrong rather than merely unhelpful.
    """
    selected: set[str] = set()
    surfaced: dict[str, str | None] = {}
    for r in rows:
        path = r.get("path") or ""
        for g in gold:
            if path == g or path.endswith("/" + g):
                surfaced[g] = r.get("discovery_source")
                if r.get("selected"):
                    selected.add(g)

    never = sorted(gold - set(surfaced))
    not_selected = sorted(set(surfaced) - selected)
    by_source = Counter(surfaced[g] or "changed-file" for g in sorted(set(surfaced)) if g in surfaced)
    return {
        "gold": len(gold),
        "selected": sorted(selected),
        "surfaced_not_selected": not_selected,
        "never_surfaced": never,
        "surfaced_by_source": dict(by_source),
    }


def render(result: dict) -> str:
    total = result["gold"]
    sel = len(result["selected"])
    sns = len(result["surfaced_not_selected"])
    never = len(result["never_surfaced"])

    out = [f"# Gold attribution over {total} gold files\n"]
    out.append("| outcome | count | fixable by |")
    out.append("|---|---|---|")
    out.append(f"| selected | {sel} | — |")
    out.append(f"| surfaced, not selected | {sns} | ranking / budget (#65, #123) |")
    out.append(f"| never surfaced | {never} | discovery (#130, #179) |")

    if result["surfaced_by_source"]:
        out.append("\nWhich strategy surfaced the gold files that made it into the universe:\n")
        out.append("| source | gold files |")
        out.append("|---|---|")
        for src, n in sorted(result["surfaced_by_source"].items(), key=lambda kv: -kv[1]):
            out.append(f"| {src} | {n} |")

    if never:
        out.append(f"\nNever surfaced: {', '.join(result['never_surfaced'][:20])}")
        if never > 20:
            out.append(f"...and {never - 20} more")
    return "\n".join(out)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dump", required=True, type=Path, help="provenance JSONL from DIFFCTX_PROVENANCE_DUMP")
    ap.add_argument("--gold", action="append", default=[], help="repo-relative gold path; repeatable")
    ap.add_argument("--out", type=Path)
    args = ap.parse_args(argv)

    if not args.gold:
        print("no --gold paths given; nothing to attribute", flush=True)
        return 1
    text = render(attribute(load_dump(args.dump.resolve()), set(args.gold)))
    if args.out:
        args.out.resolve().write_text(text)
    print(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
