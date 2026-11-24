#!/usr/bin/env python3
"""Selection scores of the corpus' diff cases from one `yaml_cases` run, and the comparison of two runs.

  o15_corpus.py parse --log yaml_cases.log --report oracle_report.json --out DIR
      DIR/scores.tsv  case, score, recall, forbidden_rate, changed-fragment share
      DIR/states.tsv  impact and symbol cases: state and per-target tp/fp/fn
  o15_corpus.py compare --a DIR --b DIR --label-a none --label-b all [--sha SHA]
      prints the table: cases scored, mean score/recall/forbidden_rate, cases that moved each way

The run sets DIFFCTX_YAML_MIN_SCORE above 100 so that every diff case without a `min_score` of its own
reports its score in a failure message, which is where the score is read from; a case with its own
`min_score` that passes prints nothing and is counted as `passed at its own threshold`, unscored.
`recall` and `forbidden_rate` come from the same message; score = recall * (1 - forbidden_rate).
Precision here is 1 - forbidden_rate: the share of the answer's forbidden probes it avoided.
"""

import argparse
import json
import re
from pathlib import Path

HEAD = re.compile(r"^---- (\S+) ----$")
SCORE = re.compile(r"score ([\d.]+)% < min [\d.]+% \(recall=([\d.]+)%, forbidden_rate=([\d.]+)%\)")
SHARE = re.compile(r"changed-fragment share: ([\d.]+)")
RESULT = re.compile(r"^test result: .*?(\d+) passed; (\d+) failed", re.M)


def parse(args):
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    text = Path(args.log).read_text(errors="replace")
    scores, name = {}, None
    for line in text.splitlines():
        m = HEAD.match(line)
        if m:
            name = m.group(1)
            continue
        m = SCORE.search(line)
        if m and name:
            scores[name] = [float(m.group(1)), float(m.group(2)), float(m.group(3)), None]
            continue
        m = SHARE.search(line)
        if m and name in scores:
            scores[name][3] = float(m.group(1))
    with open(out / "scores.tsv", "w") as f:
        f.write("case\tscore\trecall\tforbidden_rate\tchanged_share\n")
        for case, (s, r, fr, sh) in sorted(scores.items()):
            f.write(f"{case}\t{s}\t{r}\t{fr}\t{'' if sh is None else sh}\n")
    totals = RESULT.findall(text)
    passed, failed = (sum(int(t[i]) for t in totals) for i in (0, 1))
    (out / "totals.json").write_text(json.dumps({"passed": passed, "failed": failed, "scored": len(scores)}))
    report = Path(args.report)
    rows = json.loads(report.read_text()) if report.exists() else []
    cases = rows.get("cases", rows) if isinstance(rows, dict) else rows
    with open(out / "states.tsv", "w") as f:
        f.write("case\tstate\ttp\tfp\tfn\n")
        for c in cases:
            tp = sum(t["tp"] for t in c["targets"])
            fp = sum(t["fp"] for t in c["targets"])
            fn = sum(t["fn"] for t in c["targets"])
            f.write(f"{c['case']}\t{c['state']}\t{tp}\t{fp}\t{fn}\n")
    print(f"{len(scores)} scored diff cases, {len(cases)} oracle cases, {passed} passed / {failed} failed")


def read_scores(d):
    rows = {}
    for line in (Path(d) / "scores.tsv").read_text().splitlines()[1:]:
        case, s, r, fr, _sh = line.split("\t")
        rows[case] = (float(s), float(r), float(fr))
    states = {}
    for line in (Path(d) / "states.tsv").read_text().splitlines()[1:]:
        case, state, tp, fp, fn = line.split("\t")
        states[case] = (state, int(tp), int(fp), int(fn))
    return rows, states, json.loads((Path(d) / "totals.json").read_text())


def mean(values):
    values = list(values)
    return sum(values) / len(values) if values else float("nan")


def compare(args):
    (sa, _ta, tota), (sb, _tb, totb) = (read_scores(args.a)[:3], read_scores(args.b)[:3])
    la, lb = args.label_a, args.label_b
    both = sorted(set(sa) & set(sb))
    lines = [
        f"# DIFFCTX_CELL_EDGES={la} against {lb} on the corpus" + (f" at {args.sha}" if args.sha else ""),
        "",
        "| | " + la + " | " + lb + " |",
        "|---|---:|---:|",
        f"| diff cases with a printed score | {len(sa)} | {len(sb)} |",
        f"| harness totals (passed / failed) | {tota['passed']} / {tota['failed']} | {totb['passed']} / {totb['failed']} |",
        f"| mean score over the {len(both)} cases scored in both | {mean(sa[c][0] for c in both):.2f}% | {mean(sb[c][0] for c in both):.2f}% |",
        f"| mean recall | {mean(sa[c][1] for c in both):.2f}% | {mean(sb[c][1] for c in both):.2f}% |",
        f"| mean forbidden_rate (precision = 100 - this) | {mean(sa[c][2] for c in both):.2f}% | {mean(sb[c][2] for c in both):.2f}% |",
        f"| cases at score 100 | {sum(sa[c][0] >= 100 for c in sa)} | {sum(sb[c][0] >= 100 for c in sb)} |",
    ]
    better = [c for c in both if sb[c][0] > sa[c][0] + 1e-9]
    worse = [c for c in both if sb[c][0] < sa[c][0] - 1e-9]
    only_a = sorted(set(sa) - set(sb))
    only_b = sorted(set(sb) - set(sa))
    lines += [
        "",
        f"cases scoring higher with {lb}: {len(better)}; lower: {len(worse)}; unchanged: {len(both) - len(better) - len(worse)}",
        f"cases scored only with {la}: {len(only_a)}; only with {lb}: {len(only_b)} "
        "(a case that reaches its own `min_score` prints no score)",
        "",
        f"## Lower with {lb} (score {la} -> {lb})",
        "",
    ]
    lines += [f"- {c}: {sa[c][0]:.1f} -> {sb[c][0]:.1f}" for c in worse]
    lines += ["", f"## Higher with {lb}", ""]
    lines += [f"- {c}: {sa[c][0]:.1f} -> {sb[c][0]:.1f}" for c in better]
    ra, rb = read_scores(args.a)[1], read_scores(args.b)[1]
    moved = [(c, ra[c][0], rb[c][0]) for c in sorted(set(ra) & set(rb)) if ra[c][0] != rb[c][0]]
    tp = lambda r: (sum(v[1] for v in r.values()), sum(v[2] for v in r.values()), sum(v[3] for v in r.values()))  # noqa: E731
    (a1, a2, a3), (b1, b2, b3) = tp(ra), tp(rb)
    lines += [
        "",
        f"## Impact and symbol cases (oracle states): {len(ra)} with {la}, {len(rb)} with {lb}",
        "",
        f"relation targets tp/fp/fn: {la} {a1}/{a2}/{a3}, {lb} {b1}/{b2}/{b3}; cases whose state differs: {len(moved)}",
        "",
    ]
    lines += [f"- {c}: {x} -> {y}" for c, x, y in moved]
    print("\n".join(lines))


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("parse")
    p.add_argument("--log", required=True)
    p.add_argument("--report", required=True)
    p.add_argument("--out", required=True)
    p.set_defaults(fn=parse)
    c = sub.add_parser("compare")
    c.add_argument("--a", required=True)
    c.add_argument("--b", required=True)
    c.add_argument("--label-a", default="a")
    c.add_argument("--label-b", default="b")
    c.add_argument("--sha", default="")
    c.set_defaults(fn=compare)
    args = ap.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
