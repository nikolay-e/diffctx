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
import operator
import re
import tomllib
from collections import Counter, defaultdict
from dataclasses import dataclass
from pathlib import Path

import adjudication
import paired
from corpus import ROOT
from model import UNCAPPED_BUDGET

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


GATES_FILE = Path(__file__).with_name("gates.toml")
SUFFIX_OPERATORS = {"_max": "max", "_min": "min", "_below": "below"}
COMPARE = {"max": operator.le, "min": operator.ge, "below": operator.lt}
OPERATOR_TEXT = {"max": "<=", "min": ">=", "below": "<"}


@dataclass(frozen=True)
class Threshold:
    gate: str  # `section.name`: the key without its operator suffix
    op: str  # max, min, below, require or read
    limit: float | None


def load_gates(path: Path = GATES_FILE) -> dict[str, Threshold]:
    """Every threshold of the verdict by gate name: the file's `<name>_max|_min|_below` keys, `true` and "read"."""
    found: dict[str, Threshold] = {}
    for section, entries in tomllib.loads(path.read_text(encoding="utf-8")).items():
        for key, limit in entries.items():
            suffix = next((s for s in SUFFIX_OPERATORS if key.endswith(s)), None)
            if limit == "read":
                threshold = Threshold(f"{section}.{key}", "read", None)
            elif limit is True:
                threshold = Threshold(f"{section}.{key}", "require", None)
            elif suffix and isinstance(limit, int | float) and not isinstance(limit, bool):
                threshold = Threshold(f"{section}.{key.removesuffix(suffix)}", SUFFIX_OPERATORS[suffix], limit)
            else:
                raise ValueError(f"{path}: [{section}] {key} = {limit!r} is no threshold (_max, _min, _below, true or 'read')")
            found[threshold.gate] = threshold
    return found


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
        total["truth_affected"] += cell.get("truth_affected", cell["truth"])
        total["tp_affected"] += cell.get("tp_affected", cell["tp"])
        total["folded"] += cell.get("folded", 0)
        total["false_fold"] += cell.get("false_fold", 0)
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
                "truth_affected": t["truth_affected"],
                "recall_affected": _ratio(t["tp_affected"], t["truth_affected"]),
                "recall_with_candidates": _ratio(t["tp"] + t["tp_weak"], t["truth"]),
                "abstention_share": _ratio(t["abstained"], t["cells"]),
                "false_complete_negatives": t["fcn"],
                "folded": t["folded"],
                "false_fold": t["false_fold"],
                "weak_per_answer": _ratio(t["tp_weak"] + t["fp_weak"], len(answers[(language, mode, kind)])),
                "kind_mismatches": t["kind_mismatches"],
            }
        )
    return rows


@dataclass(frozen=True)
class LoadedRun:
    path: Path
    engine: str | None
    budget: int | None

    @property
    def cells(self) -> list[dict]:
        found = self.path / "cells.jsonl"
        return read_records(found) if found.exists() else []


def load_run(spec: str) -> LoadedRun:
    """`DIR` or `DIR@BUDGET`: the directory of one measure run; its `run.json` says which engine and budget made it.

    `@BUDGET` (a token count or `uncapped`) names the budget of a run measured before `run.json` existed.
    """
    directory, _, given = spec.partition("@")
    path = Path(directory)
    meta_file = path / "run.json"
    meta = json.loads(meta_file.read_text(encoding="utf-8")) if meta_file.exists() else {}
    budget = (UNCAPPED_BUDGET if given == "uncapped" else int(given)) if given else meta.get("budget")
    return LoadedRun(path, meta.get("engine"), budget)


def budget_label(budget: int | None) -> str:
    return "default" if budget is None else "uncapped" if budget >= UNCAPPED_BUDGET else str(budget)


def curve_rows(runs: list[LoadedRun]) -> list[dict]:
    """One row per (language, mode, relation, budget): what the same engine scores as its budget grows."""
    engines = {run.engine for run in runs if run.engine}
    if len(engines) > 1:
        raise ValueError(f"a budget curve is one engine at several budgets; got {sorted(engines)}")
    seen: dict[int | None, Path] = {}
    for run in runs:
        if run.budget in seen:
            raise ValueError(f"{run.path} and {seen[run.budget]} are both at budget {budget_label(run.budget)}")
        seen[run.budget] = run.path
    rows = [{**row, "budget": run.budget} for run in runs for row in scip_rows(run.cells)]
    rows.sort(key=lambda r: (r["language"], r["mode"], r["relation"], r["budget"] or 0))
    return rows


def paired_entry(run: Path, base: LoadedRun, rule: paired.Measurability) -> dict:
    head = load_run(str(run))
    return {
        "base": base.path.name,
        "head": head.path.name,
        "budgets": (base.budget, head.budget),
        "result": paired.compare_runs(base.path, head.path, rule),
    }


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


def ac5_totals(path: Path) -> dict:
    """The AC5 scorer's `totals.json`: the facts found of those expected, and the share of answer rows that are noise."""
    totals = json.loads(path.read_text(encoding="utf-8"))["all"]
    recall = totals["recall"]["all"]
    return {"found": recall["found"], "total": recall["total"], "noise_share": totals["noise"]["share"]}


Observation = tuple[str, object, str]  # source, value, what the number is made of


def observations(report: dict) -> dict[str, list[Observation]]:
    """What this run measured for each gate; a gate with no entry here had no input."""
    seen: dict[str, list[Observation]] = defaultdict(list)
    if report["scip"]:
        seen["safety.false_complete_negatives"].append(
            ("oracle cells", sum(r["false_complete_negatives"] for r in report["scip"]), "")
        )
        seen["safety.false_fold"].append(("oracle cells", sum(r["false_fold"] for r in report["scip"]), ""))
    for name, label in (("faults", "fault sweep"), ("metamorphic", "metamorphic suite")):
        if report.get(name):
            seen["safety.false_complete_negatives"].append((label, report[name]["false_complete_negatives"], ""))
    if report.get("harness"):
        found = sum(case["false_complete_negatives"] for case in report["harness"]["cases"])
        seen["safety.false_complete_negatives"].append(("YAML corpus", found, ""))
    impact = ((report.get("bench") or {}).get("all") or {}).get("impact") or {}
    if impact.get("p95") is not None:
        seen["speed.impact_p95_seconds"].append(("bench", impact["p95"], f"{impact.get('n')} runs"))
    for entry in report["paired"]:
        found = entry["result"]["measurability"]
        source = f"{entry['head']} against {entry['base']}"
        counts = f"{found['measured']} of {found['cells']} cells measured"
        widest = f"widest decided interval {found['widest']}; {found['unmeasured']} cells judged on the safety rows alone"
        seen["measurability.interval_width"].append((source, counts, widest))
        seen["measurability.lower_bound"].append((source, found["lowest"], "; ".join(found["failing"])))
    ac5 = report.get("ac5")
    if ac5:
        head, base = ac5["head"], ac5["base"]
        seen["ac5.noise"].append(("head", head["noise_share"], f"{head['noise_share']:.1%} of answer rows"))
        if base:
            same = head["total"] == base["total"]
            detail = f"head {head['found']}/{head['total']}, recomputed base {base['found']}/{base['total']}"
            detail += "" if same else "; the two runs scored different materials"
            seen["ac5.head_not_below_recomputed_base"].append(
                ("head against base", same and head["found"] >= base["found"], detail)
            )
    return seen


INFORMATIONAL = frozenset({"measurability.interval_width"})  # it sorts cells into measured or not and can never fail


def _status(gate: Threshold, value: object) -> str:
    if value is None:
        return "n/a"
    if gate.gate in INFORMATIONAL:
        return "info"
    if gate.op == "read":
        return "read"
    if gate.op == "require":
        return "pass" if value else "fail"
    return "pass" if COMPARE[gate.op](value, gate.limit) else "fail"


def _limit_text(gate: Threshold) -> str:
    if gate.op in ("read", "require"):
        return "every row read" if gate.op == "read" else "required"
    return f"{OPERATOR_TEXT[gate.op]} {gate.limit}"


def verdict_lines(report: dict, gates: dict[str, Threshold]) -> list[dict]:
    """One line per observation of each threshold in `gates`, and one `n/a` line for a threshold the run gave no input."""
    seen = observations(report)
    return [
        {
            "gate": gate.gate,
            "source": source,
            "status": _status(gate, value),
            "value": value,
            "limit": _limit_text(gate),
            "detail": detail,
        }
        for gate in gates.values()
        for source, value, detail in seen.get(gate.gate) or [("", None, "")]
    ]


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
    ("Affected", "truth_affected"),
    ("Recall (affected)", "recall_affected"),
    ("Recall+cand.", "recall_with_candidates"),
    ("Abstention", "abstention_share"),
    ("FCN", "false_complete_negatives"),
    ("Folded", "folded"),
    ("False fold", "false_fold"),
    ("Weak/answer", "weak_per_answer"),
    ("Kind mism.", "kind_mismatches"),
]
CURVE_COLUMNS = [
    ("Language", "language"),
    ("Mode", "mode"),
    ("Relation", "relation"),
    ("Budget", "budget_label"),
    ("Truth", "truth"),
    ("Recall", "recall"),
    ("Precision", "precision"),
    ("FCN", "false_complete_negatives"),
    ("Abstention", "abstention_share"),
]
PAIRED_COLUMNS = [
    ("Language", "language"),
    ("Mode", "mode"),
    ("Relation", "relation"),
    ("Commits", "commits"),
    ("Sites", "truth"),
    ("Head P", "head_precision"),
    ("Head R", "head_recall"),
    ("Precision", "precision_text"),
    ("Recall", "recall_text"),
    ("Recall (affected)", "recall_affected_text"),
    ("FCN", "fcn_text"),
    ("False fold", "false_fold_head"),
    ("State", "state"),
    ("Flag", "flag"),
]
SMALL_COLUMNS = [("From", "from"), ("To", "to"), ("Kind", "kind"), ("Commit", "commit"), ("Base", "base"), ("Head", "head")]
ADJUDICATION_COLUMNS = [
    ("Language", "language"),
    ("Adjudicated", "total"),
    ("SCIP confirmed", "confirmed"),
    ("Relabelled", "relabelled"),
    ("Refuted", "refuted"),
    ("Unresolved", "unresolved"),
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


def _signed(value: float | None) -> str:
    return "n/a" if value is None else f"{value:+.3f}"


def _change(metric: dict) -> str:
    """`base → head, Δ [low, high]`: the point change with the interval the bootstrap put around it."""
    interval = "" if metric["low"] is None else f" [{_signed(metric['low'])}, {_signed(metric['high'])}]"
    ends = ("n/a" if metric[side] is None else str(metric[side]) for side in ("base", "head"))
    return f"{' → '.join(ends)}, Δ{_signed(metric['delta'])}{interval}"


def _flag(row: dict) -> str:
    marks = [f"{name} {row[name]['verdict'].upper()}" for name in paired.METRICS if row[name]["verdict"]]
    failing = [f"{name} FAILS" for name in row["failing"]]
    return ", ".join(
        [*failing, *marks, *(["read rows"] if row["small"] else []), *(["TRUTH DIFFERS"] if row["truth_differs"] else [])]
    )


def render_paired(entry: dict) -> list[str]:
    result = entry["result"]
    parts = ["", f"## Paired bootstrap: {entry['head']} against {entry['base']}", ""]
    parts.append(
        f"Commits answered by both runs: impact {result['commits']['impact']}, symbol {result['commits']['symbol']}; dropped "
        f"(answered by one run only) base {result['dropped']['base']}, head {result['dropped']['head']}. "
        f"{paired.RESAMPLES} resamples over commits, seed {paired.SEED}, 95 % percentile interval, which needs {paired.MIN_COMMITS}+ "
        f"commits. Groups are not corrected for multiple comparisons."
    )
    rule = result["measurability"]
    parts.append(
        f"A cell is measured when its precision and recall intervals are at most {rule['width_max']} wide; a measured cell "
        f"fails when the lower bound of a measured interval is under {rule['lower_min']}. An unmeasured cell is judged on the "
        f"safety rows alone. WORSE and BETTER only say the whole interval lies below or above 0."
    )
    if entry["budgets"][0] != entry["budgets"][1]:
        parts += ["", f"**The runs used different budgets** (base {entry['budgets'][0]}, head {entry['budgets'][1]})."]
    rows = [
        {**row, "head_precision": row["precision"]["head"], "head_recall": row["recall"]["head"],
            "precision_text": _change(row["precision"]), "recall_text": _change(row["recall"]),
            "recall_affected_text": _change(row["recall_affected"]),
         "fcn_text": f"{row['fcn_base']} → {row['fcn_head']}", "flag": _flag(row)}
        for row in result["rows"]
    ]  # fmt: skip
    parts += ["", table_of(PAIRED_COLUMNS, rows, "No paired cells."), ""]
    parts.append(
        f"Cells: {rule['cells']}, measured {rule['measured']} (failing {len(rule['failing'])}), unmeasured {rule['unmeasured']}. "
        f"Failing: {'; '.join(rule['failing']) or 'none'}."
    )
    if result["small"]:
        parts += ["", f"### Relations with truth under {paired.SMALL_TRUTH}, row by row", ""]
        groups: dict[tuple, list[dict]] = defaultdict(list)
        for item in result["small"]:
            groups[(item["language"], item["mode"], item["kind"])].append(item)
        for (language, mode, kind), items in groups.items():
            parts += [f"**{language} {mode} {kind}** ({len(items)} rows)", "", table_of(SMALL_COLUMNS, items, ""), ""]
    return parts


def table_of(columns: list[tuple[str, str]], rows: list[dict], empty: str) -> str:
    if not rows:
        return empty
    return table([name for name, _ in columns], [[row.get(key) for _, key in columns] for row in rows])


def render_verdict(lines: list[dict]) -> list[str]:
    rows = [
        [
            line["gate"],
            line["source"],
            "n/a" if line["value"] is None else line["value"],
            line["limit"],
            line["status"].upper(),
            line["detail"],
        ]
        for line in lines
    ]
    failed = [line["gate"] for line in lines if line["status"] == "fail"]
    summary = f"Failing: {', '.join(failed)}." if failed else "No threshold fails; lines marked N/A had no input in this run."
    return ["", "## Verdict", "", table(["Gate", "Source", "Value", "Limit", "Verdict", "Numbers"], rows), "", summary]


def render(report: dict) -> str:
    parts = [
        "# diffctx oracle report",
        *render_verdict(report["verdict"]),
        "",
        "## SCIP truth (raw), by tier, language, mode and relation",
        "",
    ]
    parts.append(table_of(SCIP_COLUMNS, report["scip"], "No scored cells."))
    if report.get("scip_adjudicated") is not None:
        parts += ["", "## SCIP truth with the adjudicated overrides", ""]
        parts += [table_of(SCIP_COLUMNS, report["scip_adjudicated"], "No scored cells.")]
        parts += ["", "### SCIP against the adjudicated set", "", table_of(ADJUDICATION_COLUMNS, report["adjudication"], "")]
    if report.get("curve"):
        rows = [{**row, "budget_label": budget_label(row["budget"])} for row in report["curve"]]
        parts += ["", "## Budget curve (one engine, several budgets)", "", table_of(CURVE_COLUMNS, rows, "")]
    for entry in report.get("paired") or []:
        parts += render_paired(entry)
    parts += ["", "## Index completeness", "", table_of(COMPLETENESS_COLUMNS, report["index_completeness"], "No indexes.")]
    parts += ["", f"Changed definitions with no SCIP symbol (not scored): {report['unmapped_definitions']}."]
    if report.get("filtered_references"):
        parts += ["", f"SCIP references dropped by the truth filters (see filters.py): {report['filtered_references']}."]
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


def build(
    run: Path,
    harness: Path | None,
    metamorphic: Path | None,
    faults: Path | None,
    bench: Path | None,
    adjudicated: Path | None = None,
    curve: list[LoadedRun] | None = None,
    against: list[LoadedRun] | None = None,
    gates: dict[str, Threshold] | None = None,
    ac5_head: Path | None = None,
    ac5_base: Path | None = None,
) -> dict:
    def optional(name: str) -> list[dict]:
        path = run / name
        return read_records(path) if path.exists() else []

    gates = load_gates() if gates is None else gates
    rule = paired.Measurability(gates["measurability.interval_width"].limit, gates["measurability.lower_bound"].limit)
    cells = optional("cells.jsonl")
    report = {
        "scip": scip_rows(cells),
        "index_completeness": completeness_rows(optional("indexes.jsonl"), optional("commits.jsonl")),
        "unmapped_definitions": len(optional("unmapped.jsonl")),
        "filtered_references": dict(sum((Counter(c.get("filtered", {})) for c in optional("commits.jsonl")), Counter())),
        "harness": harness_rows(read_records(harness)) if harness else None,
        "metamorphic": outcome_summary(read_records(metamorphic)) if metamorphic else None,
        "faults": outcome_summary(read_records(faults)) if faults else None,
        "bench": json.loads(bench.read_text(encoding="utf-8")) if bench else None,
        "curve": curve_rows(curve) if curve else None,
        "paired": [paired_entry(run, other, rule) for other in against or []],
        "ac5": {"head": ac5_totals(ac5_head), "base": ac5_totals(ac5_base) if ac5_base else None} if ac5_head else None,
        "scip_adjudicated": None,
        "adjudication": None,
    }
    verdicts = adjudication.load_verdicts(adjudicated) if adjudicated and adjudicated.is_dir() else {}
    if verdicts:
        rows = adjudication.apply_to_rows(optional("relations.jsonl"), verdicts)
        report["scip_adjudicated"] = scip_rows(adjudication.cells_from_rows(rows, cells))
        report["adjudication"] = adjudication.summary(verdicts, repo_languages())
    report["verdict"] = verdict_lines(report, gates)
    return report


def repo_languages() -> dict[str, str]:
    from corpus import load_corpus

    return {repo.name: repo.language for repo in load_corpus().repos}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="report", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--run", type=Path, default=Path("out/latest"), help="the measure.py output directory")
    parser.add_argument("--harness", type=Path)
    parser.add_argument("--metamorphic", type=Path)
    parser.add_argument("--faults", type=Path)
    parser.add_argument("--bench", type=Path)
    parser.add_argument("--adjudicated", type=Path, default=ROOT / "adjudicated", help="evidence-backed verdicts over SCIP truth")
    parser.add_argument(
        "--curve",
        action="append",
        type=load_run,
        metavar="DIR[@BUDGET]",
        help="a measure run of one engine at one budget (repeatable): adds the budget -> recall curve",
    )
    parser.add_argument(
        "--against",
        action="append",
        type=load_run,
        metavar="DIR[@BUDGET]",
        help="a measure run on the same commits as --run (repeatable): adds the paired bootstrap of --run against it",
    )
    parser.add_argument(
        "--gates",
        type=lambda text: load_gates(Path(text)),
        metavar="FILE",
        help="the thresholds (default: gates.toml beside this file)",
    )
    parser.add_argument("--ac5-head", type=Path, help="the AC5 scorer's totals.json for the head")
    parser.add_argument("--ac5-base", type=Path, help="the same for the base, scored on the same material")
    parser.add_argument("--out", type=Path, help="directory for report.md and report.json (default: --run)")
    options = parser.parse_args(argv)
    report = build(
        options.run,
        options.harness,
        options.metamorphic,
        options.faults,
        options.bench,
        options.adjudicated,
        options.curve,
        options.against,
        options.gates,
        options.ac5_head,
        options.ac5_base,
    )
    out = options.out or options.run
    out.mkdir(parents=True, exist_ok=True)
    (out / "report.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    (out / "report.md").write_text(render(report), encoding="utf-8")
    print(f"wrote {out / 'report.md'} and {out / 'report.json'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
