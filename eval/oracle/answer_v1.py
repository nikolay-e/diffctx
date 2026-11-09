"""The `diffctx.impact.v1` reader, frozen: stored answers of 1.20.0, 09a06560, 067b3514 and w5r2 rescore through it.

It reads what v1 wrote as prose and as `path::symbol (lines)` strings. Nothing here changes with a scoring ruling: a
change to how an answer is understood goes in `answer.py` or `compare.py`, and a test holds this file's output to the
numbers it gave before v2 existed (`tests/fixtures/v1_answers.json`).
"""

from __future__ import annotations

import json
import re

from answer import ROW_KINDS, Answer, AnswerCaller, Resolver, TargetAnswer

_OMITTED = re.compile(r"^(?P<count>\d+) changed definition\(s\) omitted")
_LISTED = re.compile(r"^(?P<path>\S+?)::(?P<name>\S+) \((?P<lines>\d+-\d+)\)")
_ALSO_CHANGED = re.compile(r"^(?P<path>\S+?)::(?P<name>.+?)(?:\[\d+\])? \((?P<lines>\d+-\d+)\)$")
_ALSO_CHANGED_SPAN = re.compile(r"^(?P<path>[^\s:]+):(?P<lines>\d+(?:-\d+)?)$")  # no symbol: the engine knows only where
_NOT_TRACED = {
    "entry_unlisted": "entered by a runner or the framework: callers not traced",
    "layout_only": "changed only in comments, formatting or position: callers not traced",
}


def parse_v1(data: dict, resolve: Resolver) -> Answer:
    targets: dict[str, TargetAnswer] = {}
    unnamed = 0
    for entry in data.get("changed", []):
        key = resolve(entry["path"], entry.get("symbol"), entry.get("lines"))
        target = targets.setdefault(key, TargetAnswer(key))
        target.unresolved = entry.get("unresolved") or target.unresolved
        target.missed_in += [m.get("path", "") for m in entry.get("missed_in", [])]
        target.callers_omitted += int(entry.get("callers_omitted", 0) or 0)
        target.entry = entry.get("entry") or target.entry
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
            stated = caller.get("kind") if caller.get("kind") in ROW_KINDS else None
            target.callers.append(
                AnswerCaller(from_key, caller.get("evidence", "resolved"), relation, stated, caller.get("folded_by"))
            )
        # A use inside a definition the range changes: the engine names it beside the callers (R14).
        for text in entry.get("also_changed", []):
            found = _ALSO_CHANGED.match(str(text))
            spanned = _ALSO_CHANGED_SPAN.match(str(text))
            if found:
                key = resolve(found["path"], found["name"], found["lines"])
            elif spanned:
                by_span = getattr(resolve, "by_span", None)
                key = by_span(spanned["path"], spanned["lines"]) if by_span else resolve(spanned["path"], None, None)
            else:
                continue
            target.callers.append(AnswerCaller(key, "resolved", "also_changed"))
    # A definition the answer files under a role or a layout-only change says what it does not trace: it
    # claims no absence of callers.
    for section, why in _NOT_TRACED.items():
        for text in data.get(section, []):
            found = _LISTED.match(str(text))
            if found:
                key = resolve(found["path"], found["name"], found["lines"])
                role = targets.setdefault(key, TargetAnswer(key))
                role.unresolved = role.unresolved or why
    attributed = any(entry.get("callers_omitted") or entry.get("omitted") for entry in data.get("changed", []))
    return Answer(
        targets=targets,
        limits=[json.dumps(limit, sort_keys=True) for limit in data.get("limits", [])],
        unanalysed={u.get("path", "") for u in data.get("unanalysed_files", [])},
        empty=bool(data.get("empty", False)),
        unnamed_omitted=unnamed,
        truncated_unattributed=0 if attributed else int(data.get("truncated", 0) or 0),
        truncated=int(data.get("truncated", 0) or 0),
    )
