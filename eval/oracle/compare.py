"""Score one diffctx answer (impact or --symbol) against the oracle's relations for one target definition."""

from __future__ import annotations

from collections.abc import Callable

from answer import Answer, AnswerCaller, Resolver, TargetAnswer
from answer import parse_answer as parse_answer  # the scorer's callers read answers through this module
from answer_v2 import _ref_key
from model import MODULE, Definition, Relation, without_span

CONSTRUCTORS = {"__init__", "constructor"}

KIND_PRIORITY = ("calls", "instantiates", "passes_as_value", "writes", "reads", "type_use", "overrides", "reexports", "imports")
CALL_KINDS = {"calls", "instantiates"}
# A `reexport` row is a module that re-exports the definition: resolved evidence, relation `reexports`.
EVIDENCE_RANK = {"resolved": 2, "reexport": 2, "candidate": 1}
SURE = {"resolved", "reexport"}
# `untyped-receiver` folds a candidate: weak evidence shown as a count, no claim that the change cannot reach it.
NOT_A_CLAIM = {"untyped-receiver"}


def _line_span(lines: str | None) -> tuple[int, int] | None:
    if not lines:
        return None
    first, _, last = str(lines).partition("-")
    try:
        return int(first), int(last or first)
    except ValueError:
        return None


def _overlap(definition: Definition, span: tuple[int, int]) -> bool:
    return definition.start <= span[1] and span[0] <= definition.end


def make_resolver(*providers: Callable[[str], list[Definition]]) -> Resolver:
    """Map diffctx's (path, symbol, lines) to the AST definition key the oracle uses."""

    def resolve(path: str, symbol: str | None, lines: str | None) -> str:
        if not symbol:
            return f"{path}::{MODULE}"
        name = symbol.replace("::", ".").split(".")[-1]
        span = _line_span(lines)
        fallback = None
        for provider in providers:
            named = [d for d in provider(path) if d.name == name]
            if named:
                if span:
                    named.sort(key=lambda d: -(max(0, min(span[1], d.end) - max(span[0], d.start) + 1)))
                if len(named) == 1 or not span or _overlap(named[0], span):
                    return named[0].key
                fallback = fallback or named[0].key  # overloads, none of them at the span: another snapshot may hold it
        return fallback or f"{path}::{symbol}"

    def by_span(path: str, lines: str) -> str:
        """The definition a bare `path:lines` names: the narrowest one holding the span, else the one overlapping most."""
        span = _line_span(lines)
        for provider in providers:
            defs = provider(path)
            holding = [d for d in defs if span and d.start <= span[0] and span[1] <= d.end]
            if holding:
                return min(holding, key=lambda d: d.end - d.start).key
            overlapping = [d for d in defs if span and d.start <= span[1] and span[0] <= d.end]
            if overlapping:
                return max(overlapping, key=lambda d: min(span[1], d.end) - max(span[0], d.start)).key
        return f"{path}::{MODULE}"

    resolve.by_span = by_span  # type: ignore[attr-defined]
    return resolve


def best_kind(relations: list[Relation]) -> str:
    kinds = {r.kind for r in relations}
    return next((k for k in KIND_PRIORITY if k in kinds), "reads")


def _strongest(callers: list[AnswerCaller]) -> dict[str, AnswerCaller]:
    """One row per caller: the surest evidence, and among equals a listed row over a folded one, so a pair counts as folded
    only when every row the answer has for it is."""

    def rank(c: AnswerCaller) -> tuple[int, bool]:
        return EVIDENCE_RANK.get(c.evidence, 0), not c.folded_by

    best: dict[str, AnswerCaller] = {}
    for c in callers:
        if c.from_key not in best or rank(c) > rank(best[c.from_key]):
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
        "truth_affected": 0,  # truth pairs the folding table does not fold for this change
        "tp_affected": 0,
        "folded": 0,  # truth pairs the engine marked folded
        "false_fold": 0,  # ... of which the oracle says the change affects: the gate is 0
        "abstained": False,
        "reported": False,  # the answer has an entry for the target: only then does its silence claim anything
        "listed": False,  # the answer lists a caller for the target, inside the diff or not
        "incomplete_complete": 0,  # affected truth pairs of the cell an answer that calls itself complete lists nowhere
        "fcn": False,
    }


def incomplete_complete(complete: bool, status: str, affected: bool) -> bool:
    """The safety row: the answer is complete for (definition, relation) while a truth pair the change can reach is in
    neither its rows nor its candidates (`fn`). Silence about what the change cannot reach is no false claim."""
    return complete and status == "fn" and affected


def false_complete_negative(incomplete: int, listed: bool) -> bool:
    """A false complete negative is the special case of that row where the answer lists no caller of the definition."""
    return incomplete > 0 and not listed


def entry_for(target: Definition, answer: Answer) -> TargetAnswer | None:
    """The answer's entry for `target`; a constructor is reported as its class: its callers build the class."""
    found = answer.targets.get(target.key)
    constructor = next((name for name in CONSTRUCTORS if without_span(target.key).endswith(f".{name}")), None)
    if found is None and constructor:
        return answer.targets.get(without_span(target.key).removesuffix(f".{constructor}"))
    return found


def _in_fold_row(target: Definition, answer: Answer) -> bool:
    """The answer lists the definition as a row it folds: it names it, so it did not lose it."""
    return any(
        caller.folded_by and caller.from_key == target.key for entry in answer.targets.values() for caller in entry.callers
    )


def unlisted_named(data: dict, resolve: Resolver) -> frozenset[str]:
    """The definitions a v2 answer names in any of its `unlisted` lists (no caller, called only in the diff, entered by a
    role, layout only): it filed them apart from its entries, it did not lose them. An item is a definition reference, or
    wraps one under `def`."""
    refs = (item for listing in (data.get("unlisted") or {}).values() for item in (listing or {}).get("items", []))
    return frozenset(_ref_key(ref, resolve) for ref in (item.get("def", item) for item in refs) if ref.get("path"))


def missing_definition(target: Definition, answer: Answer, unlisted: frozenset[str] = frozenset()) -> dict | None:
    """A changed definition the answer says nothing of: no entry, no fold row naming it, no `unlisted` item naming it, no
    disclosure that its file went unanalysed. None where the answer names it.

    The record holds what classifies it: the answer's entries that may be the same definition under another key (an
    identity mismatch: same file, or the same name elsewhere) and what else the answer says about itself. With none, it is
    an engine loss.
    """
    if (
        entry_for(target, answer) is not None
        or target.key in unlisted
        or target.path in answer.unanalysed
        or _in_fold_row(target, answer)
    ):
        return None
    named = [key for key in answer.targets if without_span(key).rpartition("::")[2].split(".")[-1] == target.name]
    return {
        "target": target.key,
        "path": target.path,
        "entries": len(answer.targets),
        "entries_in_file": sorted(key for key in answer.targets if key.partition("::")[0] == target.path),
        "entries_named_alike": sorted(named),
        "answer_empty": answer.empty,
        "limits": len(answer.limits),
    }


def silent_loss(missing: list[dict], answer: Answer) -> dict:
    """The safety row of one answer: the definitions it lost without a word.

    The answer counts the definitions it shed without naming them (`unnamed_omitted`); that many of the missing ones are
    accounted for, though which ones it cannot say. The rest are silent. Every missing definition stays listed beside
    the count, so a reader sees what the number was made of.
    """
    return {
        "missing": len(missing),
        "unnamed_omitted": answer.unnamed_omitted,
        "silent": max(0, len(missing) - answer.unnamed_omitted),
        "definitions": missing,
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
    reported = entry_for(target, answer)
    callers = _strongest(reported.callers) if reported else {}
    # A symbol the cap cut callers from, or shed whole (its stub carries an "omitted ..." reason in `unresolved`),
    # is a partial answer: the listed callers count, the missing truth is abstention.
    abstained = (
        bool(answer.limits)
        or answer.truncated_unattributed > 0
        or target.path in answer.unanalysed
        or bool(reported and (reported.unresolved or reported.missed_in or reported.callers_omitted))
        or (reported is None and answer.unnamed_omitted > 0)
    )
    # `entry`, `truncated`, `callers_omitted` and `also_changed` disclose what the list leaves out: with nothing else
    # listed for the target, the answer abstains (R15). `callers_omitted` and `also_changed` are read above.
    abstained = abstained or (not callers and (answer.truncated > 0 or bool(reported and reported.entry)))
    # An answer with no entry for the target says nothing of it: a symbol it found no definition of ("named in N file(s), a
    # textual match"), a nested definition it reports under its container. Only an entry claims anything, so only one can be complete.
    complete = reported is not None and not abstained
    cells: dict[str, dict] = {}
    details: list[dict] = []

    def cell(kind: str) -> dict:
        return cells.setdefault(kind, _blank_cell(target.key, kind))

    for from_key, relations in scored.items():
        kind = best_kind(relations)
        row = cell(kind)
        row["truth"] += 1
        affected = any(r.rule is None for r in relations)
        row["truth_affected"] += affected
        caller = callers.get(from_key)
        if caller is None:
            row["fn"] += 1
            status, evidence = "fn", None
        else:
            status, evidence = ("tp" if caller.evidence in SURE else "tp_weak"), caller.evidence
            row[status] += 1
            false_fold = bool(caller.folded_by) and caller.folded_by not in NOT_A_CLAIM and affected
            row["folded"] += bool(caller.folded_by)
            row["false_fold"] += false_fold
            row["tp_affected"] += affected and status == "tp" and not caller.folded_by
            row["kind_mismatches"] += _kind_mismatch(kind, caller)
        shown = next((r for r in relations if r.kind == kind), relations[0])
        folded_by = caller.folded_by if caller else None
        incomplete = incomplete_complete(complete, status, affected)
        row["incomplete_complete"] += incomplete
        details.append(
            _detail(
                status,
                from_key,
                target,
                shown,
                kind,
                evidence,
                reported,
                affected=affected,
                folded_by=folded_by,
                incomplete=incomplete,
            )
        )
    for from_key, caller in callers.items():
        if from_key in scored or from_key in ignored:
            continue
        kind = caller.kind or {"call": "calls", "reexports": "reexports"}.get(caller.relation, "reads")
        moved = ["R14"] if caller.relation == "also_changed" else []
        if kind == "calls" and _constructs(target):
            kind, moved = "instantiates", [*moved, "R10"]  # a call that builds a class is an instantiation
        status = "fp" if caller.evidence in SURE else "fp_weak"
        cell(kind)[status] += 1
        details.append(_detail(status, from_key, target, None, kind, caller.evidence, reported, moved))
    for row in cells.values():
        row["abstained"] = abstained
        row["reported"] = reported is not None
        row["listed"] = bool(callers)
        row["fcn"] = false_complete_negative(row["incomplete_complete"], bool(callers))
    return list(cells.values()), details


def _kind_mismatch(kind: str, caller: AnswerCaller) -> bool:
    """The engine's row names another kind than the truth's: its own `kind` when it states one, else call against not-call."""
    if caller.relation == "also_changed":
        return False
    if caller.kind is not None:
        return caller.kind != kind and not (caller.kind in CALL_KINDS and kind in CALL_KINDS)
    return (kind in CALL_KINDS) != (caller.relation == "call")


def _constructs(target: Definition) -> bool:
    return target.kind == "class" or target.name in CONSTRUCTORS


def _detail(
    status: str,
    from_key: str,
    target: Definition,
    relation: Relation | None,
    kind: str,
    evidence: str | None,
    reported: TargetAnswer | None,
    moved: list[str] | None = None,
    affected: bool | None = None,
    folded_by: str | None = None,
    incomplete: bool = False,
) -> dict:
    return {
        "verdict": status,
        "from": from_key,
        "to": target.key,
        "kind": kind,
        "site": relation.site if relation else None,
        "moved": list(relation.moved) if relation else moved or [],
        "affected": affected,
        "folded_by": folded_by,
        "false_fold": bool(folded_by) and folded_by not in NOT_A_CLAIM and bool(affected),
        "incomplete_complete": incomplete,
        "rule": relation.rule if relation else None,
        "evidence": evidence,
        "target_decorated": target.decorated,
        "unresolved": reported.unresolved if reported else None,
    }
