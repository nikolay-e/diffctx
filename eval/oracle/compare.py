"""Score one diffctx answer (impact or --symbol) against the oracle's relations for one target definition."""

from __future__ import annotations

import json
import re
from collections.abc import Callable
from dataclasses import dataclass, field

from model import MODULE, Definition, Relation

KIND_PRIORITY = ("calls", "instantiates", "passes_as_value", "writes", "reads", "overrides", "reexports", "imports")
CALL_KINDS = {"calls", "instantiates"}
# A `reexport` row is a module that re-exports the definition: resolved evidence, relation `reexports`.
EVIDENCE_RANK = {"resolved": 2, "reexport": 2, "candidate": 1}
SURE = {"resolved", "reexport"}

Resolver = Callable[[str, str | None, str | None], str]


@dataclass
class AnswerCaller:
    from_key: str
    evidence: str
    relation: str  # call | reference | reexports


@dataclass
class TargetAnswer:
    key: str
    callers: list[AnswerCaller] = field(default_factory=list)
    unresolved: str | None = None
    missed_in: list[str] = field(default_factory=list)
    callers_omitted: int = 0  # callers the answer cap dropped from this symbol: its list is a lower bound


@dataclass
class Answer:
    targets: dict[str, TargetAnswer]
    limits: list[str]
    unanalysed: set[str]
    empty: bool
    # Definitions the cap shed whose names the answer also left out ("and N more"): any target it does not
    # list may be one of them.
    unnamed_omitted: int = 0


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


_OMITTED = re.compile(r"^(?P<count>\d+) changed definition\(s\) omitted")
_LISTED = re.compile(r"^(?P<path>\S+?)::(?P<name>\S+) \((?P<lines>\d+-\d+)\)")
_NOT_TRACED = {
    "entry_unlisted": "entered by a runner or the framework: callers not traced",
    "layout_only": "changed only in comments, formatting or position: callers not traced",
}


def parse_answer(data: dict, resolve: Resolver) -> Answer:
    targets: dict[str, TargetAnswer] = {}
    unnamed = 0
    for entry in data.get("changed", []):
        key = resolve(entry["path"], entry.get("symbol"), entry.get("lines"))
        target = targets.setdefault(key, TargetAnswer(key))
        target.unresolved = entry.get("unresolved") or target.unresolved
        target.missed_in += [m.get("path", "") for m in entry.get("missed_in", [])]
        target.callers_omitted += int(entry.get("callers_omitted", 0) or 0)
        # The definitions the cap shed are named on one entry, `path::name` and their lines each.
        named = entry.get("omitted", [])
        for shed in named:
            path, _, name = str(shed.get("definition", "")).partition("::")
            shed_key = resolve(path, name or None, shed.get("lines"))
            gone = targets.setdefault(shed_key, TargetAnswer(shed_key))
            gone.unresolved = entry.get("unresolved") or gone.unresolved
            gone.callers_omitted += int(shed.get("callers_omitted", 0) or 0)
        counted = _OMITTED.match(str(entry.get("unresolved") or ""))
        if counted:
            unnamed += max(0, int(counted["count"]) - len(named))
        for caller in entry.get("callers", []):
            from_key = resolve(caller["path"], caller.get("symbol"), caller.get("lines"))
            relation = caller.get("relation") if caller.get("relation") in ("reference", "reexports") else "call"
            target.callers.append(AnswerCaller(from_key, caller.get("evidence", "resolved"), relation))
    # A definition the answer files under a role or a layout-only change says what it does not trace: it
    # claims no absence of callers.
    for section, why in _NOT_TRACED.items():
        for text in data.get(section, []):
            found = _LISTED.match(str(text))
            if found:
                key = resolve(found["path"], found["name"], found["lines"])
                role = targets.setdefault(key, TargetAnswer(key))
                role.unresolved = role.unresolved or why
    return Answer(
        targets=targets,
        limits=[json.dumps(limit, sort_keys=True) for limit in data.get("limits", [])],
        unanalysed={u.get("path", "") for u in data.get("unanalysed_files", [])},
        empty=bool(data.get("empty", False)),
        unnamed_omitted=unnamed,
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
    if reported is None and target.key.endswith(".__init__"):
        # A constructor is reported as its class: its callers build the class.
        reported = answer.targets.get(target.key.removesuffix(".__init__"))
    callers = _strongest(reported.callers) if reported else {}
    # A symbol the cap cut callers from, or shed whole (its stub carries an "omitted ..." reason in `unresolved`),
    # is a partial answer: the listed callers count, the missing truth is abstention.
    abstained = (
        bool(answer.limits)
        or target.path in answer.unanalysed
        or bool(reported and (reported.unresolved or reported.missed_in or reported.callers_omitted))
        or (reported is None and answer.unnamed_omitted > 0)
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
            status, evidence = ("tp" if caller.evidence in SURE else "tp_weak"), caller.evidence
            row[status] += 1
            row["kind_mismatches"] += (kind in CALL_KINDS) != (caller.relation == "call")
        details.append(_detail(status, from_key, target, relations[0], kind, evidence, reported))
    for from_key, caller in callers.items():
        if from_key in scored or from_key in ignored:
            continue
        kind = {"call": "calls", "reexports": "reexports"}.get(caller.relation, "reads")
        status = "fp" if caller.evidence in SURE else "fp_weak"
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
        "verdict": status,
        "from": from_key,
        "to": target.key,
        "kind": kind,
        "site": relation.site if relation else None,
        "evidence": evidence,
        "target_decorated": target.decorated,
        "unresolved": reported.unresolved if reported else None,
    }
