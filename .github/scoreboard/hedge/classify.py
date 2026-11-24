#!/usr/bin/env python3
"""Per case, what each binary's answer is: confident and right, confident and wrong, hedged with a
complete list, hedged with an incomplete list.

Reads the results hedge/run.py wrote (<out>/<BIN>/<mode>/<lang>/<stem>.json) with the case's own
expectations (judge.py), and writes into <report>:

  classes.tsv      one row per case: the class of each binary and whether it is a regression
  classes.md       the 2x2 per language with denominators, the totals and the regressions by case

Definitions:
  hedged   some definition of the answer states a gap or names files it did not analyse
  right    the case's expectations hold (judge.case_red is false): relations per target, the
           changed/definitions list, coverage and unknowns as the case states them
  list     the answer's relations and changed/definitions list match the case, whatever it says
           about coverage: a hedged answer whose list is right states a limit it does not need;
           one whose list is wrong has used the hedge as the excuse
Confident answers split by `right`; hedged answers split by `list`.

Regression: BASE was confident and right and HEAD is not (hedged, or wrong). The stricter reading,
BASE right (confident or hedged) and HEAD wrong, is listed apart.
Disputed cases (the corpus marks the oracle itself as in doubt) and cases a binary failed to answer
are counted apart and sit in no cell.
"""

import argparse
import sys
from collections import Counter, defaultdict
from pathlib import Path

import yaml

sys.path.insert(0, str(Path(__file__).resolve().parent))
import compare
import judge

CLASSES = ("confident-right", "confident-wrong", "hedged-list-complete", "hedged-list-incomplete")


def hedged(n):
    return any(d["gaps"] or d["missed_in"] for d in n["defs"].values())


def classify(case, n):
    v = judge.Verdicts(case, n)
    right = not judge.case_red(v)
    bare = judge.Verdicts(case, n)
    bare.coverage, bare.unknowns = {}, None
    list_ok = not judge.case_red(bare)
    if hedged(n):
        return ("hedged-list-complete" if list_ok else "hedged-list-incomplete"), right
    return ("confident-right" if right else "confident-wrong"), right


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True, help="the run.py output directory")
    ap.add_argument("--cases-root", required=True)
    ap.add_argument("--report", required=True)
    ap.add_argument("--base", default="BASE")
    ap.add_argument("--head", default="HEAD")
    ap.add_argument("--sha", default="")
    a = ap.parse_args()
    out, root, rep = Path(a.out), Path(a.cases_root), Path(a.report)
    rep.mkdir(parents=True, exist_ok=True)
    rows, failures, disputed = [], [], []
    for mode in ("impact", "symbol"):
        for jpath in sorted((out / a.base / mode).glob("*/*.json")):
            lang, stem = jpath.parent.name, jpath.stem
            ident = f"{mode}/{lang}/{stem}"
            ypath = root / mode / lang / f"{stem}.yaml"
            if not ypath.exists():
                failures.append((ident, "no such case in the corpus"))
                continue
            case = yaml.safe_load(ypath.read_text(encoding="utf-8"))
            norms, notes = {}, {}
            for name in (a.base, a.head):
                norms[name], notes[name] = compare.load(out, name, mode, lang, stem)
            bad = [f"{name}: {notes[name]}" for name in norms if norms[name] is None]
            if bad:
                failures.append((ident, "; ".join(bad)))
                continue
            if case.get("dispute"):
                disputed.append(ident)
                continue
            cb, rb = classify(case, norms[a.base])
            ch, rh = classify(case, norms[a.head])
            rows.append(
                {
                    "case": ident,
                    "mode": mode,
                    "lang": lang,
                    "base": cb,
                    "head": ch,
                    "base_right": rb,
                    "head_right": rh,
                    "regression": cb == "confident-right" and ch != "confident-right",
                    "right_to_wrong": rb and not rh,
                }
            )
    cols = ["case", "mode", "lang", "base", "head", "base_right", "head_right", "regression", "right_to_wrong"]
    with open(rep / "classes.tsv", "w") as f:
        f.write("\t".join(cols) + "\n")
        for r in rows:
            f.write("\t".join(str(r[c]) for c in cols) + "\n")

    sha = f" at {a.sha}" if a.sha else ""
    lines = [
        f"# Confident and right, by language: {a.base} against {a.head}{sha}",
        "",
        f"cases classified: {len(rows)}; disputed in the corpus (in no cell): {len(disputed)}; "
        f"a binary failed to answer (in no cell): {len(failures)}",
        "",
    ]
    langs = sorted({r["lang"] for r in rows})
    for side, key in ((a.base, "base"), (a.head, "head")):
        lines += [
            f"## {side}",
            "",
            "| language | cases | complete and right | complete and wrong | hedged, list complete | hedged, list incomplete | confident-and-right share |",
            "|---|---:|---:|---:|---:|---:|---:|",
        ]
        for lang in [*langs, None]:
            sel = [r for r in rows if lang is None or r["lang"] == lang]
            c = Counter(r[key] for r in sel)
            share = c["confident-right"] / len(sel) if sel else 0.0
            lines.append(
                f"| {lang or 'all'} | {len(sel)} | {c['confident-right']} | {c['confident-wrong']} | "
                f"{c['hedged-list-complete']} | {c['hedged-list-incomplete']} | {share:.1%} |"
            )
        lines.append("")
    moves = Counter((r["base"], r["head"]) for r in rows)
    lines += ["## Movement between classes (rows: 1.20.0, columns: head)", "", "| from \\ to | " + " | ".join(CLASSES) + " |"]
    lines.append("|---|" + "---:|" * len(CLASSES))
    for src in CLASSES:
        lines.append(f"| {src} | " + " | ".join(str(moves[(src, dst)]) for dst in CLASSES) + " |")
    regressions = [r for r in rows if r["regression"]]
    wrong = [r for r in rows if r["right_to_wrong"]]
    lines += [
        "",
        f"## Regressions: {a.base} confident and right, {a.head} not ({len(regressions)})",
        "",
        f"of which {a.head} hedged: {sum(r['head'].startswith('hedged') for r in regressions)}, "
        f"{a.head} confident and wrong: {sum(r['head'] == 'confident-wrong' for r in regressions)}",
        "",
    ]
    lines += [f"- {r['case']}: {r['base']} -> {r['head']}" for r in regressions]
    lines += ["", f"## {a.base} right (confident or hedged), {a.head} wrong ({len(wrong)})", ""]
    lines += [f"- {r['case']}: {r['base']} -> {r['head']}" for r in wrong]
    gained = [r for r in rows if r["head"] == "confident-right" and r["base"] != "confident-right"]
    lines += ["", f"## Gained: {a.head} confident and right where {a.base} was not ({len(gained)})", ""]
    by_lang = defaultdict(int)
    for r in gained:
        by_lang[r["lang"]] += 1
    lines += [f"- {lang}: {n}" for lang, n in sorted(by_lang.items())]
    lines += ["", f"## Cases without a classification ({len(failures)})", ""]
    lines += [f"- {c}: {why}" for c, why in failures]
    lines += ["", f"## Disputed ({len(disputed)})", ""]
    lines += [f"- {c}" for c in disputed]
    (rep / "classes.md").write_text("\n".join(lines) + "\n")
    print(
        f"classified {len(rows)} cases: regressions {len(regressions)}, right->wrong {len(wrong)}, "
        f"failures {len(failures)}, disputed {len(disputed)}"
    )


if __name__ == "__main__":
    main()
