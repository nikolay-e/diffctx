"""The `diffctx.impact.v2` reader: structured gaps, `DefRef`s and ledgers, no prose to match.

Where v1 said a sentence (`unresolved`) v2 names a gap; where v1 wrote `path::symbol (lines)` v2 gives `{path, symbol,
lines}`; where v1 counted what the cap shed in a field per place v2 keeps one ledger (`shed`) and one listing
(`omitted.definitions`). The schema is described in MIGRATION.md; the rows' field names are the engine's.
"""

from __future__ import annotations

import json

from answer import ROW_KINDS, Answer, AnswerCaller, Resolver, TargetAnswer

ENTERED = "entered by a runner or the framework: callers not traced"
LAYOUT = "changed only in comments, formatting or position: callers not traced"
SHED = "omitted to fit the answer cap: callers not listed"
CLASSES = {"call": "call", "implementation": "call", "reference": "reference", "reexports": "reexports"}


def _ref_key(ref: dict, resolve: Resolver) -> str:
    """The definition a `DefRef` names; without a symbol the engine knows only where it is."""
    if ref.get("symbol"):
        return resolve(ref["path"], ref["symbol"], ref.get("lines"))
    by_span = getattr(resolve, "by_span", None)
    return by_span(ref["path"], ref.get("lines", "")) if by_span else resolve(ref["path"], None, None)


def _shed_total(shed: dict) -> int:
    return sum(int(shed.get(kind, 0) or 0) for kind in ("resolved", "candidate", "name"))


def _row(row: dict, resolve: Resolver) -> AnswerCaller:
    stated = row.get("kind") if row.get("kind") in ROW_KINDS else None
    return AnswerCaller(
        resolve(row["path"], row.get("symbol"), row.get("lines")),
        row.get("standing", "resolved"),
        CLASSES.get(row.get("class", "call"), "call"),
        stated,
        row.get("folded_by"),
    )


def parse_v2(data: dict, resolve: Resolver) -> Answer:
    targets: dict[str, TargetAnswer] = {}
    attributed = False
    for definition in data.get("definitions", []):
        key = resolve(definition["path"], definition.get("symbol"), definition.get("lines"))
        target = targets.setdefault(key, TargetAnswer(key))
        coverage = definition.get("coverage", {})
        searched = [*coverage.get("calls", {}).get("gaps", []), *coverage.get("reads", {}).get("gaps", [])]
        gaps = [*definition.get("gaps", []), *searched]
        if gaps:  # a search that left a gap says what it did not cover, in the words of the closed vocabulary
            target.unresolved = target.unresolved or ", ".join(sorted({gap["kind"] for gap in gaps}))
        target.missed_in += [f"{gap.get('files', 0)} unread file(s)" for gap in searched if gap["kind"] == "unaccounted"]
        shed = _shed_total(definition.get("shed", {}))
        target.callers_omitted += shed
        attributed = attributed or shed > 0
        target.entry = definition.get("entry") or target.entry
        target.callers += [_row(row, resolve) for row in definition.get("rows", [])]
        # A use inside a definition the range changes: structured, so it needs no parsing (R14).
        target.callers += [
            AnswerCaller(_ref_key(ref, resolve), "resolved", "also_changed") for ref in definition.get("also_changed", [])
        ]
    unnamed = 0
    omitted = data.get("omitted")
    if omitted:
        listing = omitted["definitions"]
        unnamed = max(0, int(listing["total"]) - len(listing["items"]))
        attributed = True
        for item in listing["items"]:
            gone_key = _ref_key(item["def"], resolve)
            gone = targets.setdefault(gone_key, TargetAnswer(gone_key))
            gone.unresolved = gone.unresolved or SHED
            gone.callers_omitted += _shed_total(item.get("shed", {}))
    unlisted = data.get("unlisted", {})
    # A definition the answer files under a role or a layout-only change says what it does not trace: it claims no
    # absence of callers.
    for entered in unlisted.get("entered_by", {}).get("items", []):
        role_key = _ref_key(entered["def"], resolve)
        role = targets.setdefault(role_key, TargetAnswer(role_key))
        role.unresolved = role.unresolved or ENTERED
    for ref in unlisted.get("layout_only", {}).get("items", []):
        layout_key = _ref_key(ref, resolve)
        layout = targets.setdefault(layout_key, TargetAnswer(layout_key))
        layout.unresolved = layout.unresolved or LAYOUT
    coverage = data.get("coverage", {})
    stopped = [gap for gap in coverage.get("scope", {}).get("gaps", []) if gap["kind"] == "stopped"]
    shed_all = int(data.get("summary", {}).get("shed", 0) or 0)
    return Answer(
        targets=targets,
        limits=[json.dumps(limit, sort_keys=True) for limit in [*coverage.get("limits", []), *stopped]],
        unanalysed={u.get("path", "") for u in coverage.get("unanalysed", {}).get("items", [])},
        empty=bool(data.get("empty", False)),
        unnamed_omitted=unnamed,
        truncated_unattributed=0 if attributed else shed_all,
        truncated=shed_all,
    )
