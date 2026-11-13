"""The adjudicated override layer: evidence-backed verdicts that correct SCIP's own truth.

`adjudicated/<language>.jsonl` holds references (repository, commit, caller, definition) with a `final` label
decided from evidence: a kind relabels the relation, `none` removes it, `unresolved` takes it out of precision
and recall. The layer is applied to the per-relation rows a measurement wrote, so raw-SCIP and adjudicated
numbers come from the same run.
"""

from __future__ import annotations

import json
from collections import Counter, defaultdict
from pathlib import Path

from compare import false_complete_negative, incomplete_complete, relation_group

KIND_OF = {
    "call": "calls",
    "read": "reads",
    "instantiate": "instantiates",
    "pass_as_value": "passes_as_value",
    "override": "overrides",
    "write": "writes",
    "reexport": "reexports",
    "import": "imports",
    "type_use": "type_use",
}
DECIDED = set(KIND_OF) | {"none", "unresolved"}
Key = tuple[str, str, str, str]
SHA_PREFIX = "git:"  # the stored form of a commit id: a bare 40-hex string reads as a secret to the repository's scanners


def verdict_of(row: dict) -> str:
    """tp | tp_weak | fn | fp | fp_weak of one relation row; older rows carry no verdict and it is read off the row.

    A row with a site is a relation SCIP found, without one it is a claim of diffctx's alone; no evidence means
    diffctx did not report it.
    """
    if "verdict" in row:
        return row["verdict"]
    weak = row.get("evidence") in ("candidate", "name")
    if row.get("evidence") is None:
        return "fn"
    if row.get("site") is None:
        return "fp_weak" if weak else "fp"
    return "tp_weak" if weak else "tp"


def load_verdicts(directory: Path) -> dict[Key, dict]:
    """(repo, commit, caller, definition) -> record, for every decided record under `directory`.

    A commit id is stored as `git:<sha>` and read with or without the prefix.
    """
    found: dict[Key, dict] = {}
    for path in sorted(directory.glob("*.jsonl")):
        if path.name.count(".") > 1:  # <language>.A.jsonl and the other single-pass files are inputs, not results
            continue
        for line in path.read_text(encoding="utf-8").splitlines():
            record = json.loads(line) if line.strip() else None
            if record and record.get("final") in DECIDED:
                found[(record["repo"], record["sha"].removeprefix(SHA_PREFIX), record["from"], record["def"])] = record
    return found


def apply_to_rows(rows: list[dict], verdicts: dict[Key, dict]) -> list[dict]:
    """Rows with the adjudicated truth: `none` refutes a SCIP relation, a kind relabels it, `unresolved` drops it."""
    out: list[dict] = []
    for row in rows:
        record = verdicts.get((row["repo"], row["commit"], row["from"], row["to"]))
        if record is None:
            out.append(row)
            continue
        final, verdict = record["final"], verdict_of(row)
        if final == "unresolved":
            continue
        if final == "none":
            refuted = {"tp": "fp", "tp_weak": "fp_weak"}.get(verdict, verdict)
            if verdict != "fn":
                out.append({**row, "verdict": refuted})
            continue
        confirmed = {"fp": "tp", "fp_weak": "tp_weak"}.get(verdict, verdict)
        out.append({**row, "verdict": confirmed, "kind": KIND_OF[final]})
    return out


def cells_from_rows(rows: list[dict], raw_cells: list[dict]) -> list[dict]:
    """Per (target, relation) cells rebuilt from the adjudicated rows, with the columns and the safety rows of the raw
    cell, computed by the same functions (`compare.incomplete_complete`, `compare.false_complete_negative`).

    The rows give the counts, the affected and folded columns; what belongs to the answer rather than to a relation, its
    abstention, whether it had an entry for the target or listed a caller, and the kind mismatches, is carried from the raw
    cells. A cell written before `reported` existed says it only where the answer listed a row for the target or the cell already
    was a false complete negative: such a measure undercounts the safety rows, never overcounts them.
    """
    abstained: dict[tuple, bool] = {}
    reported: dict[tuple, bool] = defaultdict(bool)
    listed: dict[tuple, bool] = defaultdict(bool)
    for c in raw_cells:
        target = (c["repo"], c["commit"], c["mode"], c["target"])
        abstained[(*target, relation_group(c["kind"]))] = c["abstained"]
        abstained[target] = abstained.get(target, False) or c["abstained"]  # for a kind relabelled into a group no raw cell has
        reported[target] |= (
            c["reported"] if "reported" in c else bool(c["fcn"] or c["tp"] + c["tp_weak"] + c["fp"] + c["fp_weak"])
        )
        listed[target] |= c.get("listed", False)
    mismatches = {(c["repo"], c["commit"], c["mode"], c["target"], c["kind"]): c["kind_mismatches"] for c in raw_cells}
    cells: dict[tuple, dict] = {}
    claimed: set[tuple] = set()
    for row in rows:
        verdict = verdict_of(row)
        target = (row["repo"], row["commit"], row["mode"], row["to"])
        cell = cells.setdefault(
            (*target, row["kind"]),
            {
                "repo": row["repo"], "language": row["language"], "commit": row["commit"], "mode": row["mode"], "target": row["to"],
                "kind": row["kind"], "truth": 0, "tp": 0, "tp_weak": 0, "fn": 0, "fp": 0, "fp_weak": 0,
                "truth_affected": 0, "tp_affected": 0, "folded": 0, "false_fold": 0, "incomplete_complete": 0, "fcn": False,
                "kind_mismatches": mismatches.get((*target, row["kind"]), 0),
                "abstained": abstained.get((*target, relation_group(row["kind"])), abstained.get(target, False)),
                "reported": reported.get(target, False), "listed": listed.get(target, False),
            },
        )  # fmt: skip
        cell[verdict] += 1
        if verdict in ("tp", "tp_weak", "fn"):
            affected = (
                row.get("affected") is not False
            )  # a relation the oracle never classified is affected: nothing folds when unsure
            cell["truth"] += 1
            cell["truth_affected"] += affected
            complete = cell["reported"] and not cell["abstained"]
            cell["incomplete_complete"] += incomplete_complete(complete, verdict, affected)
            if verdict != "fn":
                folded = bool(row.get("folded_by"))
                cell["folded"] += folded
                cell["false_fold"] += bool(row.get("false_fold"))
                cell["tp_affected"] += affected and verdict == "tp" and not folded
        if verdict != "fn":
            claimed.add(target)
    for key, cell in cells.items():
        cell["fcn"] = false_complete_negative(cell["incomplete_complete"], cell["listed"] or key[:4] in claimed)
    return list(cells.values())


def summary(verdicts: dict[Key, dict], languages: dict[str, str] | None = None) -> list[dict]:
    """How often SCIP's relation survived the adjudication: confirmed, relabelled, refuted or unresolved."""
    counts: dict[str, Counter] = defaultdict(Counter)
    for record in verdicts.values():
        language = (languages or {}).get(record["repo"], record["repo"])
        final = record["final"]
        scip = record.get("scip_kind")
        outcome = (
            "refuted"
            if final == "none"
            else "unresolved" if final == "unresolved" else "confirmed" if KIND_OF[final] == scip else "relabelled"
        )
        counts[language][outcome] += 1
        counts[language]["total"] += 1
    keys = ("total", "confirmed", "relabelled", "refuted", "unresolved")
    return [{"language": language, **{k: c[k] for k in keys}} for language, c in sorted(counts.items())]
