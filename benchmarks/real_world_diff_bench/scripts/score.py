#!/usr/bin/env python3
"""Score diffctx's actual selection against frozen gold labels.

Reads the corpus dirs produced by gen_corpus.sh (each holding ctx.yaml + meta.tsv)
and gold_labels.json, emits diffctx_realworld_bench.jsonl + summary.json.
Gold labels are the stable contract; diffctx's rendered file set is the variable
under test. Re-run after any diffctx change to re-score without re-labeling.
"""

import glob
import json
import os
import re
import sys

if len(sys.argv) < 2:
    sys.exit("usage: score.py <corpus_dir>  (dir produced by gen_corpus.sh)")
CORPUS = sys.argv[1]
HERE = os.path.dirname(os.path.abspath(__file__))
gold = {c["commit"]: c for c in json.load(open(os.path.join(HERE, "..", "gold_labels.json")))}


def norm(p):
    p = p.strip().strip('"').strip("'")
    head = p.split(":")[0]
    if ":" in p and "/" in head:
        p = head
    return p.lstrip("./")


def selected_files(d):
    y = os.path.join(d, "ctx.yaml")
    if not os.path.exists(y):
        return set()
    txt = open(y, errors="ignore").read()
    return {norm(m.group(1)) for m in re.finditer(r'^ {2}- path: "([^"]+)"', txt, re.M)}


def meta(d):
    out = {}
    with open(os.path.join(d, "meta.tsv")) as fh:
        for line in fh:
            if "\t" in line:
                key, value = line.rstrip("\n").split("\t", 1)
                out[key] = value
    return out


records = []
for d in sorted(glob.glob(f"{CORPUS}/*/*")):
    if not os.path.isdir(d):
        continue
    m = meta(d)
    cid = f"{m['repo']}/{os.path.basename(d)}"
    c = gold.get(cid, {})
    sel = selected_files(d)
    ginc = {norm(x) for x in c.get("should_include", [])}
    gexc = {norm(x) for x in c.get("should_not_include", [])}
    inter = len(sel & ginc)
    prec = inter / len(sel) if sel else 0.0
    if ginc:
        rec = inter / len(ginc)
    else:
        rec = 1.0 if not sel else 0.0
    forb = len(sel & gexc) / len(sel) if sel else 0.0
    records.append(
        {
            "commit": cid,
            "repo": m["repo"],
            "sha": m["sha"],
            "status": m["status"],
            "git_files": int(m["git_files"]),
            "md_tokens": int(m["md_tokens"]),
            "yaml_tokens": int(m["yaml_tokens"]),
            "ctx_files": int(m["ctx_files"]),
            "changed_frags": int(m["changed_frags"]),
            "gold_include": sorted(ginc),
            "gold_exclude": sorted(gexc),
            "diffctx_selected_n": len(sel),
            "precision": round(prec, 3),
            "recall": round(rec, 3),
            "forbidden_rate": round(forb, 3),
            "score": round(100 * rec * (1 - forb), 1),
        }
    )

with open(os.path.join(HERE, "..", "diffctx_realworld_bench.jsonl"), "w") as f:
    for r in records:
        f.write(json.dumps(r) + "\n")
print(f"scored {len(records)} commits")
