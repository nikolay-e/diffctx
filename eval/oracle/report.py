"""Merge the oracle's measurement, the harness report, and the metamorphic and fault outcomes into one table.

Rows are Tier x language x relation: precision, recall, abstention share, false-complete-negatives, weak
candidates per answer, and index completeness. Markdown for reading, JSON for comparing rounds.

Inputs besides the oracle run directory are read tolerantly (a JSON list, an object with a list, JSON lines
or a `cargo test` log):
  harness      DIFFCTX_ORACLE_REPORT of the YAML corpus (`diffctx.oracle-report.v1`, tests/cases/impact/SCHEMA.md):
               per case `mode`, `language`, `state`, `targets` rows {target, relation, tp, fp, fn}, `empty`,
               `disclosed`, `absence_claims`, `kind_mismatches`
  metamorphic  the `cargo test --test metamorphic` log, or records with `name`, `state`
  faults       the `cargo test --test git_faults` log, or records with `name`, `state`, `false_complete_negative`
"""

from __future__ import annotations

import argparse
import json
import re
from collections import Counter, defaultdict
from pathlib import Path

TIERS = {
    "python": 1,
    "typescript": 1,
    "rust": 1,
    "kotlin": 1,
    "go": 2,
    "java": 2,
    "swift": 2,
    "sql": 2,
    "shell": 2,
    "config": 2,
    "web": 2,
    "other": 3,
}


LIBTEST_LINE = re.compile(r"^test (\S+) \.\.\. (ok|FAILED|ignored)")
LIBTEST_STATES = {"ok": "pass", "FAILED": "fail", "ignored": "ignored"}


def parse_libtest(text: str) -> list[dict]:
    """Outcomes from the default `cargo test` output; a property that guards a known-red case passes there."""
    found = (LIBTEST_LINE.match(line) for line in text.splitlines())
    return [{"name": m.group(1), "state": LIBTEST_STATES[m.group(2)]} for m in found if m]


def read_records(path: Path) -> list[dict]:
    """A JSON list, an object holding a list, JSON lines, or a `cargo test` log."""
    text = path.read_text(encoding="utf-8").strip()
    if not text:
        return []
    outcomes = parse_libtest(text)
    if outcomes:
        return outcomes
    try:
        data = json.loads(text)
    except json.JSONDecodeError:
        return [json.loads(line) for line in text.splitlines() if line.strip()]
    if isinstance(data, dict):
        data = next((v for v in data.values() if isinstance(v, list)), [data])
    return [r for r in data if isinstance(r, dict)]


def _ratio(numerator: float, denominator: float) -> float | None:
    return None if denominator == 0 else round(numerator / denominator, 3)


def scip_rows(cells: list[dict]) -> list[dict]:
    groups: dict[tuple, dict] = defaultdict(lambda: Counter())
    answers: dict[tuple, set] = defaultdict(set)
    for cell in cells:
        key = (cell["language"], cell["mode"], cell["kind"])
        total = groups[key]
        for field in ("truth", "tp", "tp_weak", "fn", "fp", "fp_weak", "kind_mismatches"):
            total[field] += cell[field]
        total["cells"] += 1
        total["abstained"] += bool(cell["abstained"])
        total["fcn"] += bool(cell["fcn"])
        answers[key].add((cell["repo"], cell["commit"], cell["mode"]))
    rows = []
    for (language, mode, kind), t in sorted(groups.items()):
        rows.append(
            {
                "tier": TIERS.get(language, 3),
                "language": language,
                "mode": mode,
                "relation": kind,
                "cells": t["cells"],
                "truth": t["truth"],
                "precision": _ratio(t["tp"], t["tp"] + t["fp"]),
                "recall": _ratio(t["tp"], t["truth"]),
                "recall_with_candidates": _ratio(t["tp"] + t["tp_weak"], t["truth"]),
                "abstention_share": _ratio(t["abstained"], t["cells"]),
                "false_complete_negatives": t["fcn"],
                "weak_per_answer": _ratio(t["tp_weak"] + t["fp_weak"], len(answers[(language, mode, kind)])),
                "kind_mismatches": t["kind_mismatches"],
            }
        )
    return rows


def completeness_rows(indexes: list[dict], commits: list[dict]) -> list[dict]:
    by_repo: dict[str, list[dict]] = defaultdict(list)
    for entry in indexes:
        by_repo[entry["repo"]].append(entry)
    excluded: Counter = Counter(c["repo"] for c in commits if c.get("status") != "measured")
    measured: Counter = Counter(c["repo"] for c in commits if c.get("status") == "measured")
    rows = []
    for repo, entries in sorted(by_repo.items()):
        ok = [e for e in entries if e["ok"]]
        reasons = Counter(e["reason"].split(":")[0] for e in entries if not e["ok"])
        rows.append(
            {
                "repo": repo,
                "snapshots": len(entries),
                "indexed_ok": len(ok),
                "mean_file_ratio": _ratio(sum(e["completeness"]["file_ratio"] for e in ok), len(ok)),
                "mean_resolved_ratio": _ratio(sum(e["completeness"]["resolved_ratio"] for e in ok), len(ok)),
                "failures": dict(reasons),
                "commits_measured": measured[repo],
                "commits_excluded": excluded[repo],
            }
        )
    return rows


def harness_rows(records: list[dict]) -> dict[str, list[dict]]:
    """The YAML corpus report (`diffctx.oracle-report.v1`): per case, `targets` rows of {target, relation, tp, fp, fn}."""
    cases: dict[tuple, Counter] = defaultdict(Counter)
    relations: dict[tuple, Counter] = defaultdict(Counter)
    for case in records:
        mode, language = case.get("mode", "?"), case.get("language", "?")
        total = cases[(mode, language)]
        total["cases"] += 1
        total[f"state:{case.get('state', '?')}"] += 1
        missed = 0
        for target in case.get("targets") or []:
            row = relations[(mode, language, target.get("relation", "?"))]
            for field in ("tp", "fp", "fn"):
                row[field] += target.get(field, 0)
            missed += target.get("fn", 0)
        total["false_complete_negatives"] += bool(case.get("empty") and not case.get("disclosed") and missed > 0)
        total["absence_claims"] += _count(case.get("absence_claims"))
        total["kind_mismatches"] += _count(case.get("kind_mismatches"))
    return {
        "cases": [
            {
                "tier": TIERS.get(language, 3),
                "mode": mode,
                "language": language,
                "cases": t["cases"],
                "states": {k.split(":", 1)[1]: v for k, v in t.items() if k.startswith("state:")},
                "false_complete_negatives": t["false_complete_negatives"],
                "absence_claims": t["absence_claims"],
                "kind_mismatches": t["kind_mismatches"],
            }
            for (mode, language), t in sorted(cases.items())
        ],
        "relations": [
            {
                "tier": TIERS.get(language, 3),
                "mode": mode,
                "language": language,
                "relation": relation,
                "tp": t["tp"],
                "fp": t["fp"],
                "fn": t["fn"],
                "precision": _ratio(t["tp"], t["tp"] + t["fp"]),
                "recall": _ratio(t["tp"], t["tp"] + t["fn"]),
            }
            for (mode, language, relation), t in sorted(relations.items())
        ],
    }


def _count(value: object) -> int:
    if isinstance(value, list):
        return len(value)
    return int(value) if isinstance(value, int | float) else 0


def outcome_summary(records: list[dict]) -> dict:
    states = Counter(r.get("state", "?") for r in records)
    return {
        "total": len(records),
        "states": dict(states),
        "false_complete_negatives": sum(bool(r.get("false_complete_negative")) for r in records),
        "failing": sorted(r.get("name", "?") for r in records if r.get("state") == "fail"),
    }


def table(headers: list[str], rows: list[list]) -> str:
    def cell(value: object) -> str:
        return "n/a" if value is None else str(value)

    lines = ["| " + " | ".join(headers) + " |", "| " + " | ".join("---" for _ in headers) + " |"]
    lines += ["| " + " | ".join(cell(v) for v in row) + " |" for row in rows]
    return "\n".join(lines)


SCIP_COLUMNS = [
    ("Tier", "tier"),
    ("Language", "language"),
    ("Mode", "mode"),
    ("Relation", "relation"),
    ("Truth", "truth"),
    ("Precision", "precision"),
    ("Recall", "recall"),
    ("Recall+cand.", "recall_with_candidates"),
    ("Abstention", "abstention_share"),
    ("FCN", "false_complete_negatives"),
    ("Weak/answer", "weak_per_answer"),
    ("Kind mism.", "kind_mismatches"),
]
COMPLETENESS_COLUMNS = [
    ("Repo", "repo"),
    ("Snapshots", "snapshots"),
    ("Indexed", "indexed_ok"),
    ("File ratio", "mean_file_ratio"),
    ("Resolved ratio", "mean_resolved_ratio"),
    ("Failures", "failures"),
    ("Commits measured", "commits_measured"),
    ("Commits excluded", "commits_excluded"),
]
HARNESS_CASE_COLUMNS = [
    ("Tier", "tier"),
    ("Mode", "mode"),
    ("Language", "language"),
    ("Cases", "cases"),
    ("States", "states"),
    ("FCN", "false_complete_negatives"),
    ("Absence claims", "absence_claims"),
    ("Kind mism.", "kind_mismatches"),
]
HARNESS_RELATION_COLUMNS = [
    ("Tier", "tier"),
    ("Mode", "mode"),
    ("Language", "language"),
    ("Relation", "relation"),
    ("TP", "tp"),
    ("FP", "fp"),
    ("FN", "fn"),
    ("Precision", "precision"),
    ("Recall", "recall"),
]


def table_of(columns: list[tuple[str, str]], rows: list[dict], empty: str) -> str:
    if not rows:
        return empty
    return table([name for name, _ in columns], [[row.get(key) for _, key in columns] for row in rows])


def render(report: dict) -> str:
    parts = ["# diffctx oracle report", "", "## SCIP truth, by tier, language, mode and relation", ""]
    parts.append(table_of(SCIP_COLUMNS, report["scip"], "No scored cells."))
    parts += ["", "## Index completeness", "", table_of(COMPLETENESS_COLUMNS, report["index_completeness"], "No indexes.")]
    parts += ["", f"Changed definitions with no SCIP symbol (not scored): {report['unmapped_definitions']}."]
    if report.get("harness"):
        harness = report["harness"]
        parts += ["", "## YAML corpus (harness report)", "", table_of(HARNESS_CASE_COLUMNS, harness["cases"], "")]
        parts += ["", table_of(HARNESS_RELATION_COLUMNS, harness["relations"], "")]
    for name, title in (("metamorphic", "Metamorphic suite"), ("faults", "Fault sweep")):
        summary = report.get(name)
        if summary:
            parts += ["", f"## {title}", ""]
            parts.append(
                f"{summary['total']} outcomes, states {summary['states']}, "
                f"false-complete-negatives {summary['false_complete_negatives']}, failing {summary['failing'] or 'none'}."
            )
    if report.get("bench"):
        rows = [
            [op, s.get("n"), s.get("p50"), s.get("p95"), s.get("max"), s.get("errors")]
            for op, s in report["bench"]["all"].items()
        ]
        parts += ["", "## Performance baseline (seconds)", "", table(["Operation", "n", "p50", "p95", "max", "errors"], rows)]
    return "\n".join(parts) + "\n"


def build(run: Path, harness: Path | None, metamorphic: Path | None, faults: Path | None, bench: Path | None) -> dict:
    def optional(name: str) -> list[dict]:
        path = run / name
        return read_records(path) if path.exists() else []

    return {
        "scip": scip_rows(optional("cells.jsonl")),
        "index_completeness": completeness_rows(optional("indexes.jsonl"), optional("commits.jsonl")),
        "unmapped_definitions": len(optional("unmapped.jsonl")),
        "harness": harness_rows(read_records(harness)) if harness else None,
        "metamorphic": outcome_summary(read_records(metamorphic)) if metamorphic else None,
        "faults": outcome_summary(read_records(faults)) if faults else None,
        "bench": json.loads(bench.read_text(encoding="utf-8")) if bench else None,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="report", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--run", type=Path, default=Path("out/latest"), help="the measure.py output directory")
    parser.add_argument("--harness", type=Path)
    parser.add_argument("--metamorphic", type=Path)
    parser.add_argument("--faults", type=Path)
    parser.add_argument("--bench", type=Path)
    parser.add_argument("--out", type=Path, help="directory for report.md and report.json (default: --run)")
    options = parser.parse_args(argv)
    report = build(options.run, options.harness, options.metamorphic, options.faults, options.bench)
    out = options.out or options.run
    out.mkdir(parents=True, exist_ok=True)
    (out / "report.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    (out / "report.md").write_text(render(report), encoding="utf-8")
    print(f"wrote {out / 'report.md'} and {out / 'report.json'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
