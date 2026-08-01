"""Summarise a `realworld_rerun` run against the shipped 1.10.2 snapshot.

Answers the two questions the 109-commit set was built to answer, on one page:

- **Liveness.** The snapshot recorded 72 hangs out of 109 under a 30s cap, so
  its quality numbers describe the 37 survivors rather than the benchmark. The
  hang rate is therefore the first number, not a footnote — and because the
  re-run records elapsed per case, the same run can be read at the old cap too.
- **Quality.** #149's gate is "over-dump rate < 10%, precision >= 0.35" on this
  set. Both are reported over the cases that produced output, with the
  denominator stated, because a rate over survivors is not a rate over 109.

CLI:
    python -m eval.analysis.realworld_summary --runs DIR [DIR ...]
"""

from __future__ import annotations

import argparse
import json
import statistics
from collections import Counter
from pathlib import Path

SNAPSHOT = {"hang": 72, "over_dump": 34, "ok": 3, "n": 109, "mean_precision_produced": 0.176}
LEGACY_CAP_S = 30.0


def load(dirs: list[Path]) -> list[dict]:
    rows: list[dict] = []
    for d in dirs:
        f = d / "results.jsonl"
        if not f.exists():
            continue
        rows += [json.loads(line) for line in f.read_text().splitlines() if line.strip()]
    return rows


def _pct(n: int, d: int) -> str:
    return f"{n}/{d} ({100 * n / d:.0f}%)" if d else f"{n}/0"


def summarise(rows: list[dict]) -> str:
    out: list[str] = []
    n = len(rows)
    status = Counter(r.get("new_status", "unknown") for r in rows)
    produced = [r for r in rows if r.get("status") == "produced"]

    out.append(f"# real-world benchmark, {n} of {SNAPSHOT['n']} cases re-run\n")

    out.append("## Liveness\n")
    out.append("| status | this run | snapshot (1.10.2, 30s cap) |")
    out.append("|---|---|---|")
    for key in ("ok", "over_dump", "hang", "no_output", "bad_json", "checkout_fail"):
        if status.get(key) or SNAPSHOT.get(key):
            out.append(f"| {key} | {_pct(status.get(key, 0), n)} | {SNAPSHOT.get(key, '—')} |")

    # The snapshot's cap was 30s. Reading this run at that cap makes the two
    # comparable without re-running anything.
    at_legacy = sum(1 for r in produced if float(r.get("elapsed_s") or 0) > LEGACY_CAP_S)
    out.append(
        f"\n{at_legacy} of the {len(produced)} cases that produced output took longer than "
        f"{LEGACY_CAP_S:.0f}s, so at the snapshot's cap this run would read as "
        f"{status.get('hang', 0) + at_legacy} hangs."
    )

    if produced:
        el = sorted(float(r.get("elapsed_s") or 0) for r in produced)
        out.append(
            f"\nElapsed on produced cases: median {statistics.median(el):.1f}s, "
            f"p90 {el[int(0.9 * (len(el) - 1))]:.1f}s, max {el[-1]:.1f}s."
        )

    out.append("\n## Quality, over the cases that produced output\n")
    if not produced:
        out.append("No case produced output; there is nothing to score.")
        return "\n".join(out)

    over = [r for r in produced if r.get("new_status") == "over_dump"]
    out.append(f"- over-dump rate: **{_pct(len(over), len(produced))}** of produced")
    out.append(f"  (and {_pct(len(over), n)} of all {n} re-run) — gate is < 10%")

    precs = [r["precision_labelled"] for r in produced if r.get("precision_labelled") is not None]
    recs = [r["recall"] for r in produced if r.get("recall") is not None]
    if precs:
        out.append(
            f"- labelled precision: mean **{statistics.mean(precs):.3f}** over {len(precs)} scored "
            f"(snapshot {SNAPSHOT['mean_precision_produced']}) — gate is >= 0.35"
        )
    if recs:
        out.append(f"- labelled recall: mean {statistics.mean(recs):.3f} over {len(recs)} scored")

    # The budget-independent view. An absolute token threshold calls a
    # legitimately large diff an over-dump and calls a small one clean
    # regardless of how much noise it carries; the share of emitted fragments
    # that actually carry the diff does neither. The dataset's own concern logs
    # arrive at the same place ("weight changed_frags/total_frags, not just
    # absolute token count") after a reviewer found a 47k-token case where 184
    # of 195 fragments were changed — large because the diff was large.
    shares = [r["changed_frags"] / r["n_frags"] for r in produced if r.get("n_frags")]
    if shares:
        shares_sorted = sorted(shares)
        out.append(
            f"- changed-fragment share: median **{statistics.median(shares):.2f}**, "
            f"p10 {shares_sorted[int(0.1 * (len(shares_sorted) - 1))]:.2f} "
            f"(1.00 = every emitted fragment carries the diff; "
            f"{sum(1 for s in shares if s < 0.25)} of {len(shares)} below 0.25)"
        )

    toks = [r["md_tokens"] for r in produced if r.get("md_tokens")]
    if toks:
        toks.sort()
        out.append(
            f"- md tokens: median {statistics.median(toks):,.0f}, "
            f"p90 {toks[int(0.9 * (len(toks) - 1))]:,.0f}, max {toks[-1]:,.0f}"
        )

    out.append("\n## Movement against the snapshot, per case\n")
    moved = Counter()
    for r in rows:
        moved[(r.get("baseline_status"), r.get("new_status"))] += 1
    out.append("| was | now | cases |")
    out.append("|---|---|---|")
    for (was, now), count in sorted(moved.items(), key=lambda kv: -kv[1]):
        out.append(f"| {was} | {now} | {count} |")

    out.append("\n## By repo\n")
    out.append("| repo | cases | ok | over_dump | hang |")
    out.append("|---|---|---|---|---|")
    for repo in sorted({r["repo"] for r in rows if r.get("repo")}):
        rr = [r for r in rows if r.get("repo") == repo]
        s = Counter(r.get("new_status") for r in rr)
        out.append(f"| {repo} | {len(rr)} | {s.get('ok', 0)} | {s.get('over_dump', 0)} | {s.get('hang', 0)} |")

    return "\n".join(out)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", nargs="+", type=Path, required=True)
    ap.add_argument("--out", type=Path)
    args = ap.parse_args(argv)

    rows = load(args.runs)
    if not rows:
        print("no results found", flush=True)
        return 1
    text = summarise(rows)
    if args.out:
        # Resolved before writing: `--out ../../x` would otherwise land outside
        # wherever the caller thought it was pointing.
        args.out.resolve().write_text(text)
    print(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
