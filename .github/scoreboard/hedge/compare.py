#!/usr/bin/env python3
"""Hedge transitions between two diffctx binaries over the impact/symbol corpus.

Reads the result files run.py wrote (<out>/<BIN>/<mode>/<lang>/<stem>.json),
normalizes both schemas (norm.py), judges each answer against the case's own
expectations (judge.py) and writes, into <out>:

  transitions.jsonl   every transition, one JSON object per line
  transitions.tsv     the same, flat
  cases.tsv           per case and binary: state and what the case says
  summary.md          counts by label x direction x language, the REGRESSION
                      and UNJUDGED lists, binary failures, unmapped schema keys

Direction (what the change means for the reader, FROM -> TO):
  hedge    confident -> less confident (resolved -> candidate/name, complete ->
           partial, a new unknown, named -> named_key)
  unhedge  the reverse
  lost     the row (definition) is gone;  gained  it is new
  folded   resolved -> folded as not affected by a rule
Label (the case's own expectation decides; never the byte distance):
  REGRESSION   FROM matched the case, TO does not (a confident, right answer
               became a hedge, or lost a right row)
  IMPROVED     FROM did not match, TO does
  both-right / both-wrong / UNJUDGED (no expectation names the definition)
A hedge on a definition whose relations FROM got exactly right is a REGRESSION
unless the case itself expects the partial answer (coverage/unknowns) or the
candidate row.
"""

import argparse
import json
import sys
from collections import Counter, defaultdict
from pathlib import Path

import yaml

sys.path.insert(0, str(Path(__file__).resolve().parent))
import judge
import norm

CONF = {"resolved": 2, "candidate": 1, "name": 0, "unresolved": 0}


def row_state(row):
    fb = row["folded_by"]
    if fb and fb != "untyped-receiver":
        return f"folded:{fb}"
    return row["standing"] or "unresolved"


def load(out, binary, mode, lang, stem):
    """(norm, note): note is '' | 'nodef' | 'failed: ...' | 'missing'."""
    base = out / binary / mode / lang / stem
    result = base.with_suffix(".json")
    err = base.with_suffix(".err")
    if not result.exists():
        return None, "missing"
    if err.exists():
        text = err.read_text()
        if mode == "symbol" and "no definition of" in text:
            return (
                norm.normalize({"schema": "diffctx.impact.v2", "empty": True}),
                "nodef",
            )
        return None, "failed: " + text.strip().splitlines()[-1][:160]
    raw = result.read_bytes()
    if not raw.strip():
        return None, "failed: empty stdout"
    try:
        return norm.normalize(json.loads(raw)), ""
    except Exception as e:  # unreadable shapes are reported, never dropped
        return None, f"failed: normalize {type(e).__name__}: {e}"


def def_state(n, key):
    d = n["defs"].get(key)
    if d is None:
        if key in n["unlisted"]:
            return "unlisted"
        path, _, symbol = key.partition("::")
        if any(c[0] == path and c[1] == (symbol or None) and c[2] == "removed_api" for c in n["contracts"]):
            return "contract"  # a removal the answer states as a contract, not as a definition
        return "absent"
    return "partial" if (d["gaps"] or d["missed_in"]) else "complete"


def label(base_ok, head_ok):
    if base_ok is None or head_ok is None:
        return "UNJUDGED"
    return {
        (True, True): "both-right",
        (True, False): "REGRESSION",
        (False, True): "IMPROVED",
        (False, False): "both-wrong",
    }[(base_ok, head_ok)]


def both(a, b):
    vals = [v for v in (a, b) if v is not None]
    return None if not vals else all(vals)


def justified(v, d):
    """The answer's gaps are what the case itself expects: it expects partial or an
    unknown for the definition, or every gap names only sites that a candidate or
    unresolved row the case expects already covers."""
    if v.expects_partial(d):
        return True
    gaps = d["gaps"]
    if not gaps and not d["missed_in"]:
        return True
    if d["missed_in"] or any(not g.get("sites") for g in gaps):
        return False
    covered = set()
    for row in d["rows"]:
        if row["standing"] != "resolved":
            listed, ok = v.row_expected(d, row)
            if listed and ok:
                covered.add(row["path"])
    return all(site in covered for g in gaps for site in g["sites"])


def ok_for(v, d, partial):
    """Whether the answer's whole statement about definition `d` is right per the
    case: relations exact, coverage as stated, and a partial answer only when the
    case itself expects it. None when the case says nothing about `d`."""
    rel = v.relations_ok(d)
    cov = v.coverage_ok(d, partial)
    if rel is None and cov is None and not v.expects_partial(d):
        return None
    ok = both(rel, cov)
    if v.expects_partial(d) and not partial:
        return False
    if partial and not justified(v, d):
        return False
    return True if ok is None else ok


def node_of(d):
    return judge.Node(d["path"], d["symbol"])


def row_ok(v, d, row, caller):
    """True/False/None: the row (or its absence) matches the case's expectation."""
    listed = any(f == "code" and t.covers(node_of(d)) for t, f in v.listed)
    if not listed:
        return None
    if row is not None:
        return judge_row(v, d, row)
    cnode = judge.Node(caller[0], caller[1])
    return not any(w.family == "code" and w.to.covers(node_of(d)) and w.frm.covers(cnode) for w in v.wants)


def judge_row(v, d, row):
    listed, ok = v.row_expected(d, row)
    if not listed:
        return None
    if not ok and not (v.case.get("expect") or {}).get("exact", True):
        # a case that is not exact lets the answer carry rows it did not expect, unless the
        # row is a caller the case names (then its resolution is wrong) or a decoy
        who = judge.Node(row["path"], row["symbol"])
        named = any(w.family == "code" and w.to.covers(node_of(d)) and w.frm.covers(who) for w in v.wants)
        decoys = (v.case.get("expect") or {}).get("decoys") or {}
        decoy = any(
            judge.Node.target(t).covers(node_of(d))
            and any(row["path"] == p or row["path"].startswith(p.rstrip("/") + "/") for p in ps)
            for t, ps in decoys.items()
        )
        return not (named or decoy)
    return ok


def direction(a, b):
    if a == b:
        return None
    if a in (None, "absent"):
        return "gained"
    if b in (None, "absent"):
        return "lost"
    if b.startswith("folded:") and not a.startswith("folded:"):
        return "folded"
    ca, cb = CONF.get(a), CONF.get(b)
    if ca is not None and cb is not None:
        return "hedge" if cb < ca else "unhedge"
    return "other"


class _Case:
    """The transitions of one case between two normalized answers."""

    def __init__(self, case, ident, lang, mode, a, b, m):
        self.ident, self.lang, self.mode = ident, lang, mode
        self.a, self.b, self.m = a, b, m
        self.va, self.vb = judge.Verdicts(case, a), judge.Verdicts(case, b)
        self.rows_out = []

    def emit(self, kind, direc, target, frm, mid, to, a_ok, b_ok, detail=""):
        self.rows_out.append(
            {
                "case": self.ident,
                "lang": self.lang,
                "mode": self.mode,
                "kind": kind,
                "direction": direc,
                "target": target,
                "from": frm,
                "via": mid,
                "to": to,
                "label": label(a_ok, b_ok),
                "from_ok": a_ok,
                "to_ok": b_ok,
                "detail": detail,
            }
        )

    def definition_state(self, key, da, db, sa, sb, sm):
        part_a, part_b = sa == "partial", sb == "partial"
        if sa != sb and "absent" not in (sa, sb):
            if part_a != part_b:
                direc = "hedge" if part_b else "unhedge"
                self.emit(
                    "def:coverage",
                    direc,
                    key,
                    sa,
                    sm,
                    sb,
                    ok_for(self.va, da, part_a),
                    ok_for(self.vb, db, part_b),
                    _gaps(db if direc == "hedge" else da),
                )
        elif sa != sb:
            direc = "lost" if sb == "absent" else "gained"
            d = da if sb == "absent" else db
            self.emit(
                "def:presence",
                direc,
                key,
                sa,
                sm,
                sb,
                judge.presence_ok(self.va, node_of(d), sa != "absent"),
                judge.presence_ok(self.vb, node_of(d), sb != "absent"),
            )
        if part_a and part_b:
            ca = {g["cls"] for g in da["gaps"]}
            cb = {g["cls"] for g in db["gaps"]}
            fresh = sorted(cb - ca - {"partial"})
            if fresh and "other" not in ca:
                self.emit(
                    "def:new-unknown",
                    "hedge",
                    key,
                    "|".join(sorted(ca)),
                    None,
                    "|".join(sorted(cb)),
                    ok_for(self.va, da, True),
                    ok_for(self.vb, db, True),
                    f"new classes {fresh}",
                )

    def definition(self, key):
        a, b, m = self.a, self.b, self.m
        da = a["defs"].get(key) or _stub(key)
        db = b["defs"].get(key) or _stub(key)
        sa, sb = def_state(a, key), def_state(b, key)
        sm = def_state(m, key) if m else None
        self.definition_state(key, da, db, sa, sb, sm)
        rows_a = {(r["path"], r["symbol"], r["cls"]): r for r in da["rows"]}
        rows_b = {(r["path"], r["symbol"], r["cls"]): r for r in db["rows"]}
        rows_m = {(r["path"], r["symbol"], r["cls"]): r for r in (m["defs"].get(key, {}).get("rows", []) if m else [])}
        rows_b = _pair_classes(rows_a, rows_b)
        rows_m = _pair_classes(rows_a, rows_m)
        for ck in sorted(set(rows_a) | set(rows_b), key=str):
            self.row(key, ck, rows_a.get(ck), rows_b.get(ck), rows_m, da, db)
        # test links on rows present in both with identical standing
        for ck in sorted(set(rows_a) & set(rows_b), key=str):
            ra, rb = rows_a[ck], rows_b[ck]
            if row_state(ra) != row_state(rb):
                continue
            fa = sorted({t[0] or "" for t in ra["tests"]})
            fb = sorted({t[0] or "" for t in rb["tests"]})
            if fa != fb:
                self.emit(
                    "tests",
                    "lost" if len(fb) < len(fa) else "gained",
                    f"{key} <- {ck[0]}::{ck[1]}",
                    fa,
                    None,
                    fb,
                    None,
                    None,
                )

    def row(self, key, ck, ra, rb, rows_m, da, db):
        st_a = row_state(ra) if ra else "absent"
        st_b = row_state(rb) if rb else "absent"
        if st_a == st_b:
            return
        if st_a == "absent" and st_b.startswith("folded:"):
            # --unfold lists the rows the answer folds; BASE (1.20.0) has no such flag
            return
        d = db if key in self.b["defs"] else da
        self.emit(
            "row",
            direction(st_a, st_b),
            f"{key} <- {ck[0]}::{ck[1]}",
            st_a,
            (row_state(rows_m[ck]) if ck in rows_m else "absent") if self.m else None,
            st_b,
            row_ok(self.va, da if key in self.a["defs"] else d, ra, ck[:2]),
            row_ok(self.vb, d, rb, ck[:2]),
            f"sites {ra['sites'] if ra else ''} -> {rb['sites'] if rb else ''}; reason "
            f"{ra['reason'] if ra else ''} -> {rb['reason'] if rb else ''}",
        )
        ta = {tuple(t) for t in (ra or {}).get("tests", [])}
        tb = {tuple(t) for t in (rb or {}).get("tests", [])}
        if ra and rb and {t[0] for t in ta} != {t[0] for t in tb}:
            self.emit(
                "tests",
                "lost" if len(tb) < len(ta) else "gained",
                f"{key} <- {ck[0]}",
                sorted(t[0] or "" for t in ta),
                None,
                sorted(t[0] or "" for t in tb),
                None,
                None,
            )

    def references(self):
        a, b = self.a, self.b
        for pk in sorted(
            set(tuple(r[1:3]) for r in a["refs"]) | set(tuple(r[1:3]) for r in b["refs"]),
            key=str,
        ):
            ka = {r[0] for r in a["refs"] if tuple(r[1:3]) == pk}
            kb = {r[0] for r in b["refs"] if tuple(r[1:3]) == pk}
            if ka == kb:
                continue
            rkind = "stale" if "stale" in (ka | kb) else "named"
            self.emit(
                "ref",
                _ref_direction(ka, kb),
                "->".join(map(str, pk)),
                sorted(ka),
                None,
                sorted(kb),
                judge.ref_ok(self.va, rkind, pk[0], bool(ka)),
                judge.ref_ok(self.vb, rkind, pk[0], bool(kb)),
            )

    def contracts(self):
        a, b = self.a, self.b
        for ck in sorted(set(map(tuple, a["contracts"])) ^ set(map(tuple, b["contracts"])), key=str):
            in_a = list(ck) in a["contracts"]
            self.emit(
                "contract",
                "lost" if in_a else "gained",
                str(ck),
                None,
                None,
                None,
                judge.contract_ok(self.va, ck[0], in_a),
                judge.contract_ok(self.vb, ck[0], not in_a),
            )

    def run_gaps(self):
        a, b = self.a, self.b
        if set(a["run_gaps"]) != set(b["run_gaps"]):
            direc = "hedge" if len(b["run_gaps"]) > len(a["run_gaps"]) else "unhedge"
            self.emit("run", direc, self.ident, a["run_gaps"], None, b["run_gaps"], None, None)


def _ref_direction(ka, kb):
    if not ka:
        return "gained"
    if not kb:
        return "lost"
    if "named_key" in kb and "named_key" not in ka:
        return "hedge"
    if "named_key" in ka and "named_key" not in kb:
        return "unhedge"
    return "other"


def compare_case(case, lang, mode, stem, norms, src, dst, via, notes):
    a, b = norms.get(src), norms.get(dst)
    if a is None or b is None:
        return []
    c = _Case(case, f"{mode}/{lang}/{stem}", lang, mode, a, b, norms.get(via) if via else None)
    for key in sorted(set(a["defs"]) | set(b["defs"]) | set(a["unlisted"]) | set(b["unlisted"])):
        c.definition(key)
    c.references()
    c.contracts()
    c.run_gaps()
    return c.rows_out


def _pair_classes(rows_a, rows_x):
    """A caller row whose class changed between the binaries (1.20.0 said
    `reference`, the int line says `call`) is the same row: re-key the other
    side's unmatched row to FROM's class when both sides hold exactly one
    unmatched row for that (path, symbol). The class is not a standing."""
    only_a = defaultdict(list)
    for k in rows_a:
        if k not in rows_x:
            only_a[k[:2]].append(k)
    only_x = defaultdict(list)
    for k in rows_x:
        if k not in rows_a:
            only_x[k[:2]].append(k)
    out = dict(rows_x)
    for pair, ka in only_a.items():
        kx = only_x.get(pair)
        if kx and len(ka) == 1 and len(kx) == 1:
            out[ka[0]] = out.pop(kx[0])
    return out


def _stub(key):
    path, _, symbol = key.partition("::")
    return {
        "path": path,
        "symbol": symbol.split("@")[0] or None,
        "rows": [],
        "gaps": [],
        "missed_in": [],
        "change": None,
        "kind": None,
    }


def _gaps(d):
    return "; ".join(f"{g['cls']}" for g in d["gaps"]) + (f"; missed_in {d['missed_in']}" if d["missed_in"] else "")


def case_row(case, norms, ident, notes):
    out = {"case": ident}
    for name, n in norms.items():
        if n is None:
            out[name] = notes.get(name, "failed")
            continue
        v = judge.Verdicts(case, n)
        listed = sorted(v.listed, key=lambda p: (str(p[0]), p[1]))
        exact = sum(judge.target_exact(case, v.wants, v.listed, v.rels, v.folded, t, f) for t, f in listed)
        rows = [r for d in n["defs"].values() for r in d["rows"]]
        out[name] = (
            f"defs={len(n['defs'])} rows={len(rows)} "
            f"resolved={sum(r['standing'] == 'resolved' for r in rows)} "
            f"candidate={sum(r['standing'] == 'candidate' for r in rows)} "
            f"gapped={sum(bool(d['gaps'] or d['missed_in']) for d in n['defs'].values())} "
            f"targets_exact={exact}/{len(listed)}"
        )
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--cases-root", required=True)
    ap.add_argument("--report", help="where transitions/summary go (default: --out)")
    ap.add_argument("--from", dest="src", default="BASE")
    ap.add_argument("--to", dest="dst", default="HEAD")
    ap.add_argument("--via", default="")
    ap.add_argument(
        "--baseline",
        required=True,
        help="the harness's red list, to calibrate the judge port against",
    )
    a = ap.parse_args()
    out, root = Path(a.out), Path(a.cases_root)
    names = [p.name for p in out.iterdir() if p.is_dir()]
    if a.src not in names or a.dst not in names:
        sys.exit(f"{a.src} or {a.dst} has no results in {out}")
    via = a.via if a.via in names else None
    transitions, case_rows, unmapped = [], [], defaultdict(Counter)
    failures, judged = [], Counter()
    red_list = set()
    if Path(a.baseline).exists():
        for line in Path(a.baseline).read_text().splitlines():
            token = line.split("#")[0].strip()
            if token.startswith(("impact/", "symbol/")):
                red_list.add(token)
    calib = defaultdict(list)
    for mode in ("impact", "symbol"):
        for jpath in sorted((out / a.src / mode).glob("*/*.json")):
            lang, stem = jpath.parent.name, jpath.stem
            ident = f"{mode}/{lang}/{stem}"
            ypath = root / mode / lang / f"{stem}.yaml"
            case = yaml.safe_load(ypath.read_text(encoding="utf-8"))
            norms, notes = {}, {}
            for name in [a.src, a.dst] + ([via] if via else []):
                norms[name], notes[name] = load(out, name, mode, lang, stem)
                if norms[name] is None:
                    failures.append((ident, name, notes[name]))
                else:
                    for k in norms[name]["unmapped"]:
                        unmapped[name][k] += 1
            case_rows.append(case_row(case, norms, ident, notes))
            if norms.get(a.dst) is not None and not case.get("dispute"):
                mine = judge.case_red(judge.Verdicts(case, norms[a.dst]))
                calib[(ident in red_list, mine)].append(ident)
            if case.get("dispute"):
                judged["disputed"] += 1
            transitions += compare_case(case, lang, mode, stem, norms, a.src, a.dst, via, notes)
            judged["cases"] += 1
    rep = Path(a.report) if a.report else out
    rep.mkdir(parents=True, exist_ok=True)
    write(rep, a, transitions, case_rows, failures, unmapped, judged, calib)


def write(out, a, transitions, case_rows, failures, unmapped, judged, calib):
    with open(out / "transitions.jsonl", "w") as f:
        for t in transitions:
            f.write(json.dumps(t, default=str) + "\n")
    cols = [
        "case",
        "kind",
        "direction",
        "label",
        "target",
        "from",
        "via",
        "to",
        "detail",
    ]
    with open(out / "transitions.tsv", "w") as f:
        f.write("\t".join(cols) + "\n")
        for t in transitions:
            f.write("\t".join(str(t.get(c, "")).replace("\t", " ").replace("\n", " ") for c in cols) + "\n")
    names = sorted({k for r in case_rows for k in r if k != "case"})
    with open(out / "cases.tsv", "w") as f:
        f.write("\t".join(["case", *names]) + "\n")
        for r in case_rows:
            f.write("\t".join(str(r.get(c, "")) for c in ["case", *names]) + "\n")
    lines = [
        f"# Hedge transitions {a.src} -> {a.dst}" + (f" (via {a.via})" if a.via else ""),
        "",
        f"cases compared: {judged['cases']} (disputed in the corpus: {judged['disputed']}); "
        f"transitions: {len(transitions)}; binary failures: {len(failures)}",
        "",
    ]
    by = Counter((t["direction"], t["label"]) for t in transitions)
    lines += ["## direction x label", "", "| direction | label | n |", "|---|---|---|"]
    lines += [f"| {d} | {lab} | {n} |" for (d, lab), n in sorted(by.items())]
    lines += [
        "",
        "## by language (hedge direction only)",
        "",
        "| language | REGRESSION | UNJUDGED | both-right | BASE-wrong (both-wrong/IMPROVED) |",
        "|---|---|---|---|---|",
    ]
    langs = sorted({t["lang"] for t in transitions})
    for lang in langs:
        c = Counter(t["label"] for t in transitions if t["lang"] == lang and t["direction"] == "hedge")
        lines.append(
            f"| {lang} | {c['REGRESSION']} | {c['UNJUDGED']} | {c['both-right']} | " f"{c['IMPROVED'] + c['both-wrong']} |"
        )
    for title, wanted in (
        ("REGRESSION", {"REGRESSION"}),
        ("UNJUDGED hedges", {"UNJUDGED"}),
    ):
        rows = [t for t in transitions if t["label"] in wanted and (title == "REGRESSION" or t["direction"] == "hedge")]
        lines += ["", f"## {title} ({len(rows)})", ""]
        lines += [
            f"- {t['case']} :: {t['kind']} {t['direction']} {t['target']}: " f"{t['from']} -> {t['to']} {t['detail']}"
            for t in rows
        ]
    lines += ["", f"## binary failures ({len(failures)})", ""]
    lines += [f"- {c} [{b}] {n}" for c, b, n in failures]
    lines += [
        "",
        f"## judge port vs the harness red list, on {a.dst}",
        "",
        "`--to` should be the build the red list belongs to; against any other it only " "measures how far that build moved.",
        "",
        "| harness red-listed | port red | cases |",
        "|---|---|---|",
    ]
    for (listed, mine), cases in sorted(calib.items()):
        lines.append(f"| {listed} | {mine} | {len(cases)} |")
    for (listed, mine), cases in sorted(calib.items()):
        if listed != mine:
            lines += ["", f"harness red-listed={listed}, port red={mine}:"]
            lines += [f"- {c}" for c in cases]
    lines += ["", "## schema keys the normalizer did not map", ""]
    for name, counter in sorted(unmapped.items()):
        lines.append(f"- {name}: " + (", ".join(f"{k} x{n}" for k, n in sorted(counter.items())) or "none"))
    (out / "summary.md").write_text("\n".join(lines) + "\n")
    print(f"{len(transitions)} transitions over {judged['cases']} cases -> {out}/summary.md")


if __name__ == "__main__":
    main()
