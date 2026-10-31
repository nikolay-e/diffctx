"""Sample the hard relations for the evidence-backed adjudication set (T14).

The set holds references, never code: repository, commit, path, line and the observed disagreement. Two
blind passes fill `label_a`/`evidence_a` and `label_b`/`evidence_b` and a final decision fills `final`
among evidenced options only; that labelling happens in wave 2 with the trace tooling in `trace/`.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import random
from collections import defaultdict
from pathlib import Path

CLASSES = ("dynamic_dispatch", "framework_registration", "re_export", "disagreement")
INDEX_FILES = ("__init__.py", "index.ts", "index.tsx", "index.js", "index.mjs", "index.cjs")
SCIP_SEES = {"tp", "tp_weak", "fn"}
DIFFCTX_SEES = {"tp", "tp_weak", "fp", "fp_weak"}


def classify(row: dict) -> str | None:
    """The first hard class a relation belongs to, or None when SCIP and diffctx agree on an easy one."""
    if row["kind"] == "overrides" or "calls through" in (row.get("unresolved") or ""):
        return "dynamic_dispatch"
    if row.get("target_decorated"):
        return "framework_registration"
    from_path = row["from"].rsplit("::", 1)[0]
    if row["kind"] == "reexports" or (row["kind"] == "imports" and from_path.endswith(INDEX_FILES)):
        return "re_export"
    return "disagreement" if row["status"] != "tp" else None


def record(row: dict, hard: str, language: str) -> dict:
    from_path = row["from"].rsplit("::", 1)[0]
    ident = hashlib.sha256(f"{row['repo']}|{row['commit']}|{row['from']}|{row['to']}|{row['kind']}".encode()).hexdigest()[:12]
    return {
        "id": ident,
        "language": language,
        "class": hard,
        "repo": row["repo"],
        "sha": row["commit"],
        "mode": row["mode"],
        "path": from_path,
        "line": row.get("site"),
        "from": row["from"],
        "to": row["to"],
        "kind": row["kind"],
        "observed": {"scip": row["status"] in SCIP_SEES, "diffctx": row["status"] in DIFFCTX_SEES, "status": row["status"]},
        "label_a": None,
        "evidence_a": None,
        "label_b": None,
        "evidence_b": None,
        "final": None,
        "final_evidence": None,
    }


def sample(rows: list[dict], per_language: int, seed: str) -> dict[str, list[dict]]:
    """About `per_language` records per language, equal parts per hard class; a short class's share goes to the others."""
    pools: dict[str, dict[str, list[dict]]] = defaultdict(lambda: {c: [] for c in CLASSES})
    for row in rows:
        hard = classify(row)
        if hard is not None:
            pools[row["language"]][hard].append(record(row, hard, row["language"]))
    chosen: dict[str, list[dict]] = {}
    for language, by_class in sorted(pools.items()):
        rng = random.Random(f"{seed}:{language}")
        picked: list[dict] = []
        remaining = per_language
        for position, hard in enumerate(CLASSES):
            pool = sorted(by_class[hard], key=lambda r: r["id"])
            share = remaining // (len(CLASSES) - position)
            take = rng.sample(pool, min(share, len(pool)))
            picked += take
            remaining -= len(take)
        chosen[language] = sorted(picked, key=lambda r: r["id"])
    return chosen


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="adjudicate", description=__doc__)
    parser.add_argument("--run", type=Path, default=Path("out/latest"))
    parser.add_argument("--out", type=Path, default=Path("adjudicated"))
    parser.add_argument("--limit", type=int, default=200, help="records over all languages")
    parser.add_argument("--seed", default="adjudication")
    options = parser.parse_args(argv)
    rows = [json.loads(line) for line in (options.run / "relations.jsonl").read_text(encoding="utf-8").splitlines() if line]
    languages = sorted({r["language"] for r in rows})
    chosen = sample(rows, options.limit // max(1, len(languages)), options.seed)
    options.out.mkdir(parents=True, exist_ok=True)
    for language, records in chosen.items():
        (options.out / f"{language}.jsonl").write_text(
            "".join(json.dumps(r, sort_keys=True) + "\n" for r in records), encoding="utf-8"
        )
        print(f"{language}: {len(records)} records")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
