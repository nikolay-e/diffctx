"""Score one diffctx answer (impact or --symbol) against the oracle's relations for one target definition."""

from __future__ import annotations

import json
from collections.abc import Callable
from dataclasses import dataclass, field

from model import MODULE, Definition, Relation

KIND_PRIORITY = ("calls", "instantiates", "passes_as_value", "writes", "reads", "overrides", "reexports", "imports")
CALL_KINDS = {"calls", "instantiates"}
EVIDENCE_RANK = {"resolved": 2, "candidate": 1}

Resolver = Callable[[str, str | None, str | None], str]


@dataclass
class AnswerCaller:
    from_key: str
    evidence: str
    relation: str  # call | reference


@dataclass
class TargetAnswer:
    key: str
    callers: list[AnswerCaller] = field(default_factory=list)
    unresolved: str | None = None
    missed_in: list[str] = field(default_factory=list)


@dataclass
class Answer:
    targets: dict[str, TargetAnswer]
    limits: list[str]
    unanalysed: set[str]
    empty: bool


def _line_span(lines: str | None) -> tuple[int, int] | None:
    if not lines:
        return None
    first, _, last = str(lines).partition("-")
    try:
        return int(first), int(last or first)
    except ValueError:
        return None


def make_resolver(*providers: Callable[[str], list[Definition]]) -> Resolver:
    """Map diffctx's (path, symbol, lines) to the AST definition key the oracle uses."""

    def resolve(path: str, symbol: str | None, lines: str | None) -> str:
        if not symbol:
            return f"{path}::{MODULE}"
        name = symbol.replace("::", ".").split(".")[-1]
        span = _line_span(lines)
        for provider in providers:
            named = [d for d in provider(path) if d.name == name]
            if named:
                if span:
                    named.sort(key=lambda d: -(max(0, min(span[1], d.end) - max(span[0], d.start) + 1)))
                return named[0].key
        return f"{path}::{symbol}"

    return resolve


def parse_answer(data: dict, resolve: Resolver) -> Answer:
    targets: dict[str, TargetAnswer] = {}
    for entry in data.get("changed", []):
        key = resolve(entry["path"], entry.get("symbol"), entry.get("lines"))
        target = targets.setdefault(key, TargetAnswer(key))
        target.unresolved = entry.get("unresolved") or target.unresolved
        target.missed_in += [m.get("path", "") for m in entry.get("missed_in", [])]
        for caller in entry.get("callers", []):
            from_key = resolve(caller["path"], caller.get("symbol"), caller.get("lines"))
            relation = "reference" if caller.get("relation") == "reference" else "call"
            target.callers.append(AnswerCaller(from_key, caller.get("evidence", "resolved"), relation))
    return Answer(
        targets=targets,
        limits=[json.dumps(limit, sort_keys=True) for limit in data.get("limits", [])],
        unanalysed={u.get("path", "") for u in data.get("unanalysed_files", [])},
        empty=bool(data.get("empty", False)),
    )


def best_kind(relations: list[Relation]) -> str:
    kinds = {r.kind for r in relations}
    return next((k for k in KIND_PRIORITY if k in kinds), "reads")


def _strongest(callers: list[AnswerCaller]) -> dict[str, AnswerCaller]:
    best: dict[str, AnswerCaller] = {}
    for c in callers:
        if c.from_key not in best or EVIDENCE_RANK.get(c.evidence, 0) > EVIDENCE_RANK.get(best[c.from_key].evidence, 0):
            best[c.from_key] = c
    return best


def _blank_cell(target: str, kind: str) -> dict:
    return {
        "target": target,
        "kind": kind,
        "truth": 0,
        "tp": 0,
        "tp_weak": 0,
        "fn": 0,
        "fp": 0,
        "fp_weak": 0,
        "kind_mismatches": 0,
        "abstained": False,
        "fcn": False,
    }


def compare(target: Definition, truth: list[Relation], answer: Answer) -> tuple[list[dict], list[dict]]:
    """Per-kind cells for `target` and one detail row per relation pair, for adjudication.

    A pair is (caller, target). Pairs whose every truth relation sits inside the diff are not scored, in either
    direction: diffctx reports callers outside the diff.
    """
    grouped: dict[str, list[Relation]] = {}
    for relation in truth:
        grouped.setdefault(relation.from_key, []).append(relation)
    ignored = {k for k, rels in grouped.items() if all(r.in_diff for r in rels)}
    scored = {k: [r for r in rels if not r.in_diff] for k, rels in grouped.items() if k not in ignored}
    reported = answer.targets.get(target.key)
    callers = _strongest(reported.callers) if reported else {}
    abstained = (
        bool(answer.limits) or target.path in answer.unanalysed or bool(reported and (reported.unresolved or reported.missed_in))
    )
    cells: dict[str, dict] = {}
    details: list[dict] = []

    def cell(kind: str) -> dict:
        return cells.setdefault(kind, _blank_cell(target.key, kind))

    for from_key, relations in scored.items():
        kind = best_kind(relations)
        row = cell(kind)
        row["truth"] += 1
        caller = callers.get(from_key)
        if caller is None:
            row["fn"] += 1
            status, evidence = "fn", None
        else:
            status, evidence = ("tp" if caller.evidence == "resolved" else "tp_weak"), caller.evidence
            row[status] += 1
            row["kind_mismatches"] += (kind in CALL_KINDS) != (caller.relation == "call")
        details.append(_detail(status, from_key, target, relations[0], kind, evidence, reported))
    for from_key, caller in callers.items():
        if from_key in scored or from_key in ignored:
            continue
        kind = "calls" if caller.relation == "call" else "reads"
        status = "fp" if caller.evidence == "resolved" else "fp_weak"
        cell(kind)[status] += 1
        details.append(_detail(status, from_key, target, None, kind, caller.evidence, reported))
    complete_negative = bool(scored) and not callers and not abstained
    for row in cells.values():
        row["abstained"] = abstained
        row["fcn"] = complete_negative and row["truth"] > 0
    return list(cells.values()), details


def _detail(
    status: str,
    from_key: str,
    target: Definition,
    relation: Relation | None,
    kind: str,
    evidence: str | None,
    reported: TargetAnswer | None,
) -> dict:
    return {
        "status": status,
        "from": from_key,
        "to": target.key,
        "kind": kind,
        "site": relation.site if relation else None,
        "evidence": evidence,
        "target_decorated": target.decorated,
        "unresolved": reported.unresolved if reported else None,
    }
