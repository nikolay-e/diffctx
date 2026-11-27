"""Blind rounds: counters of a measured run and nothing of its rows, so a holdout can be measured once and not read.

`counters` reduces one run directory (what `measure` wrote) to sums per language and mode, and per repository to the
counts coverage is judged by: no commit, no target, no pair, no file name. `run.py blind` joins the counters of many
jobs and gates them with `gates.toml` as the full report does, through the same verdict lines.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import Counter, defaultdict
from pathlib import Path

import report
from compare import COUNTING_BASIS
from gates import load_gates
from measure import RUN_FILE
from tables import budget_label, table

SCHEMA = "diffctx.oracle-blind.v1"
GROUP_COLUMNS = ["Language", "Mode", "Cells", "Incomplete complete", "False fold", "Answers", "Missing", "Silent"]


def counters(run: Path) -> dict:
    """The counters of one run directory; the rows stay where they are."""
    meta = json.loads((run / RUN_FILE).read_text(encoding="utf-8"))
    groups: dict[tuple[str, str], Counter] = defaultdict(Counter)
    for cell in report.read_json_lines(run / "cells.jsonl"):
        total = groups[(cell["language"], cell["mode"])]
        total["cells"] += 1
        total["counted"] += "incomplete_complete" in cell
        total["incomplete_complete"] += cell.get("incomplete_complete", 0)
        total["false_fold"] += cell.get("false_fold", 0)
    for answer in report.read_json_lines(run / "silent_loss.jsonl"):
        total = groups[(answer["language"], answer["mode"])]
        total["answers"] += 1
        total["missing"] += answer["missing"]
        total["silent"] += answer["silent"]
    commits = report.read_json_lines(run / "commits.jsonl")
    snapshots = report.read_json_lines(run / "indexes.jsonl")
    languages = {c["repo"]: c["language"] for c in commits if "language" in c}
    repos = sorted({c["repo"] for c in commits} | {s["repo"] for s in snapshots})
    return {
        "schema": SCHEMA,
        "engine": meta.get("engine"),
        "budget": meta.get("budget"),
        "fresh": bool(meta.get("fresh")),
        "groups": [{"language": language, "mode": mode, **dict(total)} for (language, mode), total in sorted(groups.items())],
        "repos": [
            {
                "repo": repo,
                "language": languages.get(repo, "unknown"),
                "snapshots": sum(s["repo"] == repo for s in snapshots),
                "indexed_ok": sum(s["repo"] == repo and s["ok"] for s in snapshots),
                "commits": sum(c["repo"] == repo for c in commits),
                "measured": sum(c["repo"] == repo and c.get("status") == "measured" for c in commits),
            }
            for repo in repos
        ],
    }


FIELDS = ("cells", "counted", "incomplete_complete", "false_fold", "answers", "missing", "silent")


def join(parts: list[dict]) -> dict[str, dict]:
    """The counters of every job, per budget; parts of two engines, or of a run without the fresh mark, are refused."""
    engines = {part["engine"] for part in parts}
    if len(engines) > 1:
        raise SystemExit(f"the parts were measured by more than one engine: {sorted(map(str, engines))}")
    if not all(part["fresh"] for part in parts):
        raise SystemExit("a part was not measured by an engine's own answers (no fresh mark): it decides nothing")
    budgets: dict[str, dict] = {}
    for part in parts:
        joined = budgets.setdefault(budget_label(part["budget"]), {"engine": part["engine"], "groups": {}, "repos": []})
        for group in part["groups"]:
            total = joined["groups"].setdefault((group["language"], group["mode"]), Counter())
            total.update({field: group.get(field, 0) for field in FIELDS})
        joined["repos"] += part["repos"]
    return budgets


def verdict(joined: dict) -> list[dict]:
    """The full report's verdict lines over counters: safety rows by their totals, coverage floors by repository and lane."""
    totals = Counter()
    for group in joined["groups"].values():
        totals.update(group)
    lanes: dict[str, Counter] = defaultdict(Counter)
    for repo in joined["repos"]:
        lanes[repo["language"]].update({"commits_total": repo["commits"], "commits_measured": repo["measured"]})
    summary = {
        "scip": [{"false_fold": totals["false_fold"]}],
        "incomplete_complete": (
            {"count": totals["incomplete_complete"], "basis": COUNTING_BASIS, "listing": []} if totals["counted"] else None
        ),
        "silent_loss": (
            {"answers": totals["answers"], "missing": totals["missing"], "silent": totals["silent"]}
            if totals["answers"]
            else None
        ),
        "index_completeness": [
            {
                "repo": r["repo"],
                "indexed_share": round(r["indexed_ok"] / r["snapshots"], 3) if r["snapshots"] else None,
                "indexed_ok": r["indexed_ok"],
                "snapshots": r["snapshots"],
            }
            for r in joined["repos"]
        ],
        "lanes": [
            {**lane, "language": language, "measured_share": lane["commits_measured"] / lane["commits_total"]}
            for language, lane in sorted(lanes.items())
            if lane["commits_total"]
        ],
        "paired": [],
        "fresh": True,
    }
    lines = report.verdict_lines(summary, load_gates(), [])
    return [
        {**line, "detail": "pairs are not listed in a blind round"} if line["gate"] == "safety.incomplete_complete" else line
        for line in lines
        if line["gate"].startswith(("safety.", "coverage."))
    ]


def render(budgets: dict[str, dict], verdicts: dict[str, list[dict]]) -> str:
    parts = ["# diffctx oracle report (blind: counters only, no row)"]
    for label, joined in sorted(budgets.items()):
        rows = [
            [
                language,
                mode,
                *(group[field] for field in ("cells", "incomplete_complete", "false_fold", "answers", "missing", "silent")),
            ]
            for (language, mode), group in sorted(joined["groups"].items())
        ]
        parts += ["", f"## Budget: {label} (engine {joined['engine']})", "", table(GROUP_COLUMNS, rows)]
        parts += report.render_verdict(verdicts[label])
    return "\n".join(parts) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="blind", description=__doc__)
    parser.add_argument("--out", type=Path, required=True, help="directory for blind.json and blind.md")
    parser.add_argument("--gate", action="store_true", help="exit 1 when a threshold fails or a coverage floor is not met")
    parser.add_argument("counters", nargs="+", type=Path, help="counters files, or directories holding them")
    options = parser.parse_args(argv)
    files = sorted(f for path in options.counters for f in ([path] if path.is_file() else path.rglob("*.json")))
    budgets = join([json.loads(f.read_text(encoding="utf-8")) for f in files])
    verdicts = {label: verdict(joined) for label, joined in budgets.items()}
    options.out.mkdir(parents=True, exist_ok=True)
    document = {
        "schema": SCHEMA,
        "budgets": {
            label: {
                "engine": joined["engine"],
                "groups": [{"language": k[0], "mode": k[1], **dict(v)} for k, v in sorted(joined["groups"].items())],
                "repos": joined["repos"],
                "verdict": verdicts[label],
            }
            for label, joined in budgets.items()
        },
    }
    (options.out / "blind.json").write_text(json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    (options.out / "blind.md").write_text(render(budgets, verdicts), encoding="utf-8")
    stopped = [f"{label} {r}" for label, lines in verdicts.items() for r in report.gate_failures(lines)] if options.gate else []
    for reason in stopped:
        print(f"GATE: {reason}", file=sys.stderr)
    return 1 if stopped else 0
