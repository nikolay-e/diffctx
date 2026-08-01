"""The honest ceiling for reading a fusion result: what EGO and BM25 reach together.

Fusion is worth having only if it beats what its two components already reach
between them. Comparing RRF against either alone answers a different question,
and comparing it against the v2 standalone-BM25 number answers a worse one —
that run had its own discovery, so the gap conflates ranking with candidate
supply.

This computes the union over the *same* candidate universe, from two runs of the
same manifest that differ only in scoring mode:

    ceiling(instance) = union of the gold files EGO found and the gold files internal-bm25 found

That is an oracle: it credits a file if either mode surfaced it, with no
mechanism able to pick which. Fusion cannot exceed it, and a fusion result at or
near it has nothing left to gain from better ranking — the remaining loss is
candidate supply, which is #130's problem rather than #125's.

Reads raw `*.checkpoint.jsonl`, not sweep artifacts: `aggregate_sweep` drops
`selected_files` from its per-instance projection on purpose (the lists dominate
memory when a whole sweep is held at once), and the union cannot be computed
without them.

CLI:
    python -m eval.analysis.union_ceiling --ego DIR --lexical DIR [--fusion DIR]
"""

from __future__ import annotations

import argparse
import glob
import json
import statistics
from pathlib import Path


def load_run(d: Path) -> dict[str, dict]:
    """Per-instance rows keyed by instance id, from every checkpoint under `d`."""
    out: dict[str, dict] = {}
    for f in glob.glob(str(d / "**" / "*.checkpoint.jsonl"), recursive=True):
        with open(f) as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                r = json.loads(line)
                iid = r.get("instance_id")
                if iid:
                    out[iid] = r
    return out


def _gold_and_selected(row: dict) -> tuple[set[str], set[str]]:
    extra = row.get("extra") or {}
    return set(extra.get("gold_files") or []), set(extra.get("selected_files") or [])


def _recall(found: set[str], gold: set[str]) -> float | None:
    return len(found & gold) / len(gold) if gold else None


def ceiling(ego: dict[str, dict], lex: dict[str, dict], fusion: dict[str, dict] | None) -> dict:
    """Per-instance recalls for each arm and for their union."""
    shared = sorted(set(ego) & set(lex))
    rows = []
    for iid in shared:
        gold, ego_sel = _gold_and_selected(ego[iid])
        _, lex_sel = _gold_and_selected(lex[iid])
        if not gold:
            continue
        row = {
            "instance_id": iid,
            "ego": _recall(ego_sel, gold),
            "lexical": _recall(lex_sel, gold),
            "union": _recall(ego_sel | lex_sel, gold),
        }
        if fusion and iid in fusion:
            _, fus_sel = _gold_and_selected(fusion[iid])
            row["fusion"] = _recall(fus_sel, gold)
        rows.append(row)
    return {"instances": len(rows), "rows": rows}


def render(result: dict) -> str:
    rows = result["rows"]
    if not rows:
        return "No instance carried both a gold set and a selection in both runs."

    def mean(key: str) -> float | None:
        vals = [r[key] for r in rows if r.get(key) is not None]
        return statistics.mean(vals) if vals else None

    out = [f"# Union ceiling over {result['instances']} shared instances\n"]
    out.append("| arm | mean gold recall |")
    out.append("|---|---|")
    for key, label in (("ego", "EGO alone"), ("lexical", "internal-BM25 alone"), ("union", "**union (ceiling)**")):
        m = mean(key)
        if m is not None:
            out.append(f"| {label} | {m:.3f} |")
    fus = mean("fusion")
    if fus is not None:
        out.append(f"| RRF fusion | {fus:.3f} |")

    union_m, ego_m = mean("union"), mean("ego")
    if union_m is not None and ego_m is not None:
        headroom = union_m - ego_m
        out.append(
            f"\nHeadroom over EGO is **{headroom:+.3f}**: that is the most any "
            f"re-ranking of the same two signals can add. A fusion result below "
            f"EGO is losing on ranking; one at the ceiling has nothing left to "
            f"gain from ranking at all."
        )
        if fus is not None:
            captured = (fus - ego_m) / headroom if headroom > 1e-9 else None
            if captured is not None:
                out.append(
                    f"RRF captures **{captured:.0%}** of that headroom "
                    f"(negative means it ranks worse than EGO alone despite the "
                    f"wider candidate set)."
                )

    only_lex = sum(1 for r in rows if (r.get("lexical") or 0) > (r.get("ego") or 0))
    out.append(
        f"\n{only_lex} of {len(rows)} instances have the lexical arm ahead of EGO — "
        f"the cases where fusion has something to contribute at all."
    )
    return "\n".join(out)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--ego", required=True, type=Path, help="run dir for scoring=ego")
    ap.add_argument("--lexical", required=True, type=Path, help="run dir for scoring=bm25 (internal, NOT the external baseline)")
    ap.add_argument("--fusion", type=Path, help="optional run dir for scoring=rrf")
    ap.add_argument("--out", type=Path)
    args = ap.parse_args(argv)

    ego = load_run(args.ego.resolve())
    lex = load_run(args.lexical.resolve())
    fusion = load_run(args.fusion.resolve()) if args.fusion else None
    if not ego or not lex:
        print("no checkpoint rows found in one of the runs", flush=True)
        return 1

    text = render(ceiling(ego, lex, fusion))
    if args.out:
        args.out.resolve().write_text(text)
    print(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
