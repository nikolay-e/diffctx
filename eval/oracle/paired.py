"""Two measure runs on the same commits, compared: a paired bootstrap over commits, never a bare difference.

Per (language, mode, relation) the unit that is resampled is the commit (a commit's targets share one diff, one
index and one budget), the same commit in both runs; 2 000 resamples with a seed fixed per group give a 95 %
percentile interval of the change in precision and in recall.

A cell is measured when the intervals of both are at most `Measurability.width_max` wide, and a measured cell
fails when the lower bound of a measured interval falls under `lower_min`. A cell the data cannot pin down is
unmeasured and is judged by the safety rows alone: a statistic over a handful of commits decides nothing. Relations
whose truth is thin (under `SMALL_TRUTH`) also have their rows listed for reading.
"""

from __future__ import annotations

import json
import random
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path

RESAMPLES = 2000
SEED = 20261106
SMALL_TRUTH = 30
MIN_COMMITS = 5  # fewer paired commits collapse the interval onto the point estimate
TAIL = 0.025
METRICS = ("precision", "recall", "recall_affected")
Group = tuple[str, str, str]  # (language, mode, relation)
CommitKey = tuple[str, str]  # (repo, commit)
ERROR_FIELD = {"impact": "impact_error", "symbol": "symbol_error"}


@dataclass(frozen=True)
class Measurability:
    """What makes a cell measured and what fails a measured one; the numbers come from gates.toml."""

    width_max: float
    lower_min: float


@dataclass(frozen=True)
class Counts:
    tp: int = 0
    fp: int = 0
    truth: int = 0
    fcn: int = 0
    truth_affected: int = 0
    tp_affected: int = 0
    false_fold: int = 0


def _read_lines(path: Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def usable_commits(commits: list[dict]) -> dict[str, set[CommitKey]]:
    """Per mode, the commits whose answer was read: measured, and the engine ran without error in that mode."""
    found: dict[str, set[CommitKey]] = {mode: set() for mode in ERROR_FIELD}
    for record in commits:
        if record.get("status") != "measured":
            continue
        for mode, field in ERROR_FIELD.items():
            if not record.get(field):
                found[mode].add((record["repo"], record["commit"]))
    return found


def commit_counts(cells: list[dict], allowed: dict[str, set[CommitKey]]) -> dict[Group, dict[CommitKey, Counts]]:
    grouped: dict[Group, dict[CommitKey, Counts]] = defaultdict(dict)
    for cell in cells:
        commit = (cell["repo"], cell["commit"])
        if commit not in allowed.get(cell["mode"], ()):
            continue
        slot = grouped[(cell["language"], cell["mode"], cell["kind"])]
        before = slot.get(commit, Counts())
        slot[commit] = Counts(
            before.tp + cell["tp"],
            before.fp + cell["fp"],
            before.truth + cell["truth"],
            before.fcn + bool(cell["fcn"]),
            before.truth_affected + cell.get("truth_affected", cell["truth"]),
            before.tp_affected + cell.get("tp_affected", cell["tp"]),
            before.false_fold + cell.get("false_fold", 0),
        )
    return grouped


def _ratio(numerator: int, denominator: int) -> float | None:
    return None if denominator == 0 else numerator / denominator


@dataclass(frozen=True)
class Interval:
    low: float
    high: float

    def verdict(self) -> str:
        return "worse" if self.high < 0 else "better" if self.low > 0 else ""


def _interval(values: list[float]) -> Interval | None:
    if len(values) < RESAMPLES // 10:
        return None
    values.sort()
    return Interval(values[int(TAIL * len(values))], values[min(len(values) - 1, int((1 - TAIL) * len(values)))])


def bootstrap(rows: list[tuple[int, int, int, int, int, int]], seed: str) -> dict[str, Interval | None]:
    """Intervals of Δprecision and Δrecall over resampled commits.

    Each row is one commit: base tp, fp, truth and head tp, fp, truth. A resample where a ratio has no
    denominator in either run does not count towards that ratio.
    """
    rng = random.Random(seed)
    precision: list[float] = []
    recall: list[float] = []
    for _ in range(RESAMPLES):
        b_tp, b_fp, b_truth, h_tp, h_fp, h_truth = map(sum, zip(*rng.choices(rows, k=len(rows)), strict=True))
        b_p, h_p = _ratio(b_tp, b_tp + b_fp), _ratio(h_tp, h_tp + h_fp)
        b_r, h_r = _ratio(b_tp, b_truth), _ratio(h_tp, h_truth)
        if b_p is not None and h_p is not None:
            precision.append(h_p - b_p)
        if b_r is not None and h_r is not None:
            recall.append(h_r - b_r)
    return {"precision": _interval(precision), "recall": _interval(recall)}


def _delta(base: float | None, head: float | None) -> float | None:
    return None if base is None or head is None else head - base


def _round(value: float | None) -> float | None:
    return None if value is None else round(value, 3)


def _decided(metric: dict, rule: Measurability) -> bool:
    """The interval is there and, as printed, no wider than the rule allows."""
    return metric["low"] is not None and round(metric["high"] - metric["low"], 3) <= rule.width_max


def cell_state(row: dict, rule: Measurability) -> tuple[str, list[str]]:
    """`measured`, `failing` or `unmeasured`, and the metrics that fail.

    Measured needs both precision and recall decided; failing is a decided metric whose lower bound is under the floor.
    """
    if not all(_decided(row[name], rule) for name in ("precision", "recall")):
        return "unmeasured", []
    failing = [name for name in METRICS if _decided(row[name], rule) and row[name]["low"] < rule.lower_min]
    return ("failing" if failing else "measured"), failing


def _row(group: Group, base: dict[CommitKey, Counts], head: dict[CommitKey, Counts], rule: Measurability) -> dict:
    commits = sorted(set(base) | set(head))
    pairs = [(base.get(c, Counts()), head.get(c, Counts())) for c in commits]
    b = Counts(*(sum(getattr(p[0], f) for p in pairs) for f in Counts.__dataclass_fields__))
    h = Counts(*(sum(getattr(p[1], f) for p in pairs) for f in Counts.__dataclass_fields__))
    language, mode, relation = group
    row: dict = {
        "language": language,
        "mode": mode,
        "relation": relation,
        "commits": len(commits),
        "truth": b.truth,
        "truth_differs": b.truth != h.truth,
        "fcn_base": b.fcn,
        "fcn_head": h.fcn,
        "false_fold_head": h.false_fold,
        "small": b.truth < SMALL_TRUTH,
    }
    intervals: dict[str, Interval | None] = {"precision": None, "recall": None}
    if len(commits) >= MIN_COMMITS:
        samples = [(x.tp, x.fp, x.truth, y.tp, y.fp, y.truth) for x, y in pairs]
        intervals = bootstrap(samples, f"{SEED}:{language}:{mode}:{relation}")
    if len(commits) >= MIN_COMMITS:
        affected = [(x.tp_affected, 0, x.truth_affected, y.tp_affected, 0, y.truth_affected) for x, y in pairs]
        intervals["recall_affected"] = bootstrap(affected, f"{SEED}:{language}:{mode}:{relation}:affected")["recall"]
    for metric, base_value, head_value in (
        ("precision", _ratio(b.tp, b.tp + b.fp), _ratio(h.tp, h.tp + h.fp)),
        ("recall", _ratio(b.tp, b.truth), _ratio(h.tp, h.truth)),
        ("recall_affected", _ratio(b.tp_affected, b.truth_affected), _ratio(h.tp_affected, h.truth_affected)),
    ):
        interval = intervals.get(metric)
        row[metric] = {
            "base": _round(base_value),
            "head": _round(head_value),
            "delta": _round(_delta(base_value, head_value)),
            "low": _round(interval.low) if interval else None,
            "high": _round(interval.high) if interval else None,
            "verdict": interval.verdict() if interval else "",
        }
    row["state"], row["failing"] = cell_state(row, rule)
    return row


def _verdicts(details: list[dict], allowed: dict[str, set[CommitKey]]) -> dict[tuple, str]:
    return {
        (d["repo"], d["commit"], d["mode"], d["kind"], d["from"], d["to"]): d["verdict"]
        for d in details
        if (d["repo"], d["commit"]) in allowed.get(d["mode"], ())
    }


def small_relation_rows(
    rows: list[dict], base: dict[tuple, str], head: dict[tuple, str], languages: dict[str, str]
) -> list[dict]:
    """Every (from, to, kind) of a group with thin truth, with the verdict each run gave it ('-' when it gave none)."""
    small = {(r["language"], r["mode"], r["relation"]) for r in rows if r["small"]}
    listed = []
    for key in sorted(set(base) | set(head)):
        repo, commit, mode, kind, source, target = key
        if (languages.get(repo, "?"), mode, kind) in small:
            listed.append(
                {
                    "language": languages.get(repo, "?"),
                    "mode": mode,
                    "kind": kind,
                    "repo": repo,
                    "commit": commit[:10],
                    "from": source,
                    "to": target,
                    "base": base.get(key, "-"),
                    "head": head.get(key, "-"),
                }
            )
    return listed


def measurability_summary(rows: list[dict], rule: Measurability) -> dict:
    """How many cells the data pins down, which of them fail, and the widest and lowest interval that decided them."""
    measured = [r for r in rows if r["state"] != "unmeasured"]
    decided = [r[name] for r in measured for name in METRICS if _decided(r[name], rule)]
    return {
        "width_max": rule.width_max,
        "lower_min": rule.lower_min,
        "cells": len(rows),
        "measured": len(measured),
        "unmeasured": len(rows) - len(measured),
        "failing": [f"{r['language']} {r['mode']} {r['relation']}: {name}" for r in rows for name in r["failing"]],
        "widest": max((round(m["high"] - m["low"], 3) for m in decided), default=None),
        "lowest": min((m["low"] for m in decided), default=None),
    }


def compare_runs(base: Path, head: Path, rule: Measurability) -> dict:
    """The paired comparison of two measure output directories over the commits both answered."""
    base_commits, head_commits = _read_lines(base / "commits.jsonl"), _read_lines(head / "commits.jsonl")
    languages = {c["repo"]: c["language"] for c in (*base_commits, *head_commits) if c.get("language")}
    base_ok, head_ok = usable_commits(base_commits), usable_commits(head_commits)
    shared = {mode: base_ok[mode] & head_ok[mode] for mode in ERROR_FIELD}
    base_counts = commit_counts(_read_lines(base / "cells.jsonl"), shared)
    head_counts = commit_counts(_read_lines(head / "cells.jsonl"), shared)
    groups = sorted(set(base_counts) | set(head_counts))
    rows = [_row(g, base_counts.get(g, {}), head_counts.get(g, {}), rule) for g in groups]
    base_details = _verdicts(_read_lines(base / "relations.jsonl"), shared)
    head_details = _verdicts(_read_lines(head / "relations.jsonl"), shared)
    return {
        "commits": {mode: len(shared[mode]) for mode in shared},
        "dropped": {
            "base": {mode: len(base_ok[mode] - shared[mode]) for mode in shared},
            "head": {mode: len(head_ok[mode] - shared[mode]) for mode in shared},
        },
        "rows": rows,
        "measurability": measurability_summary(rows, rule),
        "small": small_relation_rows(rows, base_details, head_details, languages),
    }
