"""Normalize a diffctx impact JSON answer of either schema into one shape.

1.20.0 speaks `diffctx.impact.v1` (changed[].callers[], changed[].unresolved);
the 2.0.0 line speaks `diffctx.impact.v2` (definitions[].rows[], gaps,
coverage). The mapping is in schema.md. Every top-level key and every
definition/row key the normalizer does not read lands in `unmapped`, so a
field is never dropped silently.

Normal form:
  schema, empty, tests_known
  defs: {key: {path, symbol, change, rows, gaps, missed_in}}
    row: {path, symbol, cls, standing, sites, reason, folded_by, tests}
    gap: {cls, text}
  unlisted: sorted ["path::symbol"]        definitions named but with no rows
  removed: sorted ["path::symbol" | path]
  refs: sorted [[kind, path, target]]      stale / named-by / collection reads
  contracts: sorted [[path, symbol, kind]]
  run_gaps: sorted [str]                   limits, unanalysed, withheld, ...
  unmapped: sorted [str]
"""

import re

V1_CONSUMED = {
    "schema",
    "name",
    "range",
    "changed_files",
    "commit_count",
    "changed",
    "empty",
    "tests_known",
    "symbol",
    "untested_definitions",
    "untested_unlisted",
    "untested_called_in_diff",
    "layout_only",
    "layout_only_omitted",
    "contracts",
    "contracts_omitted",
    "stale_references",
    "stale_references_omitted",
    "collection_reads",
    "collection_reads_omitted",
    "limits",
    "withheld_files",
    "unanalysed_files",
    "unanalysed_files_omitted",
    "truncated",
    "changed_files_omitted",
    "untracked_files",
    "untracked_files_omitted",
    "deleted_files",
    "commits",
    "named_by",
}
V1_DEF_CONSUMED = {
    "path",
    "symbol",
    "lines",
    "kind",
    "callers",
    "unresolved",
    "missed_in",
    "compatible_callers",
}
V1_CALLER_CONSUMED = {
    "path",
    "symbol",
    "lines",
    "weight",
    "evidence",
    "relation",
    "reason",
    "sites",
    "tested_by",
    "tested_via",
    "file_tested",
    "registered_by",
    "unread_test",
}
V2_CONSUMED = {
    "schema",
    "name",
    "range",
    "definitions",
    "contracts",
    "coverage",
    "empty",
    "fit",
    "inventory",
    "provenance",
    "summary",
    "tests",
    "unlisted",
    "symbol",
    "stale",
    "named",
    "deleted_modules",
    "omitted",
    "spelled",
}
V2_DEF_CONSUMED = {
    "path",
    "symbol",
    "lines",
    "kind",
    "change",
    "rows",
    "coverage",
    "gaps",
    "shed",
    "missed_in",
    "folded",
    "entry",
}
V2_ROW_CONSUMED = {
    "path",
    "symbol",
    "lines",
    "sites",
    "class",
    "standing",
    "reason",
    "guard",
    "folded_by",
}

# 1.20.0 states a gap as one sentence; the 2.0.0 line gives it a kind.
V1_GAP_CLASSES = [
    (r"too common to trace", "common_word"),
    (r"tell them apart", "ambiguous"),
    (r"receiver the binding model cannot type", "unresolved_reads"),
    (r"no binding model", "not_modelled"),
    (r"calls through|dyn `|generic bound", "dispatched"),
]
COMMON_WORD_LINES = 500


def v1_gap_class(text):
    for pattern, cls in V1_GAP_CLASSES:
        if re.search(pattern, text):
            return cls
    return "other"


def v2_gap_class(gap):
    kind = gap.get("kind")
    if kind == "unlanded":
        return "common_word" if (gap.get("uses") or 0) > COMMON_WORD_LINES else "unresolved_reads"
    return kind or "other"


def _v2_gap(g):
    return {
        "cls": v2_gap_class(g),
        "text": g.get("kind"),
        "sites": sorted({x.get("path", "") for x in g.get("sites") or []}),
    }


def _node(entry):
    if isinstance(entry, str):
        text = re.sub(r"\s*\(\d+(-\d+)?\)\s*$", "", entry)
        return text
    path = entry.get("path", "")
    symbol = entry.get("symbol")
    return f"{path}::{symbol}" if symbol else path


def _tests_v1(caller):
    out = []
    tested = caller.get("tested_by")
    for t in ([tested] if isinstance(tested, str) else (tested or [])):
        out.append([t if isinstance(t, str) else t.get("path", ""), None])
    return out


def _tests_v2(row):
    guard = row.get("guard") or {}
    if guard.get("state") != "linked":
        return []
    tests = [[guard.get("test"), guard.get("name")]]
    for extra in guard.get("also") or []:
        tests.append([extra.get("test"), extra.get("name")])
    return tests


def _add_def(defs, entry):
    key = f"{entry['path']}::{entry['symbol']}"
    if key in defs:
        key = f"{key}@{entry.get('lines')}"
    defs[key] = entry
    return key


def normalize(doc):
    schema = doc.get("schema", "")
    return _v1(doc) if schema.endswith(".v1") else _v2(doc)


def _unmapped(prefix, obj, consumed, out):
    for key in obj:
        if key not in consumed:
            out.add(f"{prefix}{key}")


def _v1(doc):
    unmapped, defs, run_gaps = set(), {}, set()
    _unmapped("", doc, V1_CONSUMED, unmapped)
    for ch in doc.get("changed") or []:
        _unmapped("changed[].", ch, V1_DEF_CONSUMED, unmapped)
        rows = []
        for c in ch.get("callers") or []:
            _unmapped("changed[].callers[].", c, V1_CALLER_CONSUMED, unmapped)
            relation = c.get("relation")
            rows.append(
                {
                    "path": c.get("path", ""),
                    "symbol": c.get("symbol"),
                    "cls": {
                        "reference": "reference",
                        "implementation": "implementation",
                        "reexports": "reexports",
                    }.get(relation, "call"),
                    "standing": c.get("evidence"),
                    "sites": c.get("sites") or [],
                    "reason": c.get("reason"),
                    "folded_by": None,
                    "tests": _tests_v1(c),
                }
            )
        gaps = []
        unresolved = ch.get("unresolved")
        for text in ([unresolved] if isinstance(unresolved, str) else (unresolved or [])):
            gaps.append({"cls": v1_gap_class(text), "text": text})
        missed = [m.get("path", "") if isinstance(m, dict) else m for m in ch.get("missed_in") or []]
        entry = {
            "path": ch.get("path", ""),
            "symbol": ch.get("symbol"),
            "change": "removed" if ch.get("kind") == "removed" else "edited",
            "kind": ch.get("kind"),
            "rows": rows,
            "gaps": gaps,
            "missed_in": missed,
        }
        _add_def(defs, entry)
    unlisted = set()
    for key in ("untested_unlisted", "untested_called_in_diff", "layout_only"):
        for item in doc.get(key) or []:
            unlisted.add(_node(item))
    for key, name in (
        ("limits", "limits"),
        ("withheld_files", "withheld"),
        ("unanalysed_files", "unanalysed"),
        ("truncated", "truncated"),
    ):
        if doc.get(key):
            run_gaps.add(name)
    refs = set()
    for item in doc.get("stale_references") or []:
        refs.add(("stale", item.get("path", ""), item.get("target", "")))
    for item in doc.get("named_by") or []:
        refs.add(("named", item.get("path", ""), item.get("target", "")))
    for item in doc.get("collection_reads") or []:
        refs.add(("collection", item.get("path", ""), item.get("collection", "")))
    contracts = {(c.get("path", ""), c.get("symbol"), c.get("kind")) for c in doc.get("contracts") or []}
    removed = {k for k, d in defs.items() if d["change"] == "removed"}
    return {
        "schema": doc.get("schema"),
        "empty": bool(doc.get("empty")),
        "tests_known": bool(doc.get("tests_known")),
        "defs": defs,
        "unlisted": sorted(unlisted),
        "removed": sorted(removed),
        "refs": sorted(map(list, refs)),
        "contracts": sorted(map(list, contracts), key=str),
        "run_gaps": sorted(run_gaps),
        "unmapped": sorted(unmapped),
    }


def _items(value):
    if isinstance(value, dict):
        return value.get("items") or []
    return value or []


def _v2_row(r):
    return {
        "path": r.get("path", ""),
        "symbol": r.get("symbol"),
        "cls": r.get("class") or "call",
        "standing": r.get("standing"),
        "sites": r.get("sites") or [],
        "reason": r.get("reason"),
        "folded_by": r.get("folded_by"),
        "tests": _tests_v2(r),
    }


def _v2_gaps(d):
    gaps = [_v2_gap(g) for g in d.get("gaps") or []]
    coverage = d.get("coverage") or {}
    for kind in ("calls", "reads"):
        cov = coverage.get(kind) or {}
        for g in cov.get("gaps") or []:
            if not any(_v2_gap(g) == known for known in gaps):
                gaps.append(_v2_gap(g))
        if cov.get("complete") is False and not cov.get("gaps"):
            gaps.append({"cls": "partial", "text": f"{kind} partial, no gap named", "sites": []})
    return gaps


def _v2_definition(d, unmapped):
    _unmapped("definitions[].", d, V2_DEF_CONSUMED, unmapped)
    rows = []
    for r in d.get("rows") or []:
        _unmapped("definitions[].rows[].", r, V2_ROW_CONSUMED, unmapped)
        rows.append(_v2_row(r))
    return {
        "path": d.get("path", ""),
        "symbol": d.get("symbol"),
        "change": (d.get("change") or {}).get("kind"),
        "kind": d.get("kind"),
        "rows": rows,
        "gaps": _v2_gaps(d),
        "missed_in": [m.get("path", "") for m in d.get("missed_in") or []],
    }


UNLISTED_LISTS = ("no_caller", "called_in_diff", "layout_only")
UNLISTED_KEYS = {*UNLISTED_LISTS, "entered_by", "layout_only_omitted"}


def _v2_unlisted(doc, unmapped):
    unlisted = set()
    unl = doc.get("unlisted") or {}
    for key in UNLISTED_LISTS:
        for item in _items(unl.get(key)):
            unlisted.add(_node(item))
    for item in _items(unl.get("entered_by")):
        unlisted.add(_node(item.get("def") or {}))
    unmapped.update(f"unlisted.{key}" for key in unl if key not in UNLISTED_KEYS)
    return unlisted


def _v2_run_gaps(doc):
    run_gaps = set()
    cov = doc.get("coverage") or {}
    if _items(cov.get("limits")):
        run_gaps.add("limits")
    # `coverage.scope` summarizes these same gaps, so it is no gap of its own. Changed code
    # outside every definition (`outside_definitions`) is a disclosure 1.20.0 never made:
    # "untracked", apart from a file that was not read ("unanalysed").
    for item in _items(cov.get("unanalysed")):
        run_gaps.add("untracked" if item.get("reason") == "outside_definitions" else "unanalysed")
    if (cov.get("unanalysed") or {}).get("total") and not _items(cov.get("unanalysed")):
        run_gaps.add("unanalysed")
    if (doc.get("inventory") or {}).get("withheld"):
        run_gaps.add("withheld")
    for key in ("spelled", "omitted"):
        if doc.get(key):
            run_gaps.add(key)
    return run_gaps


def _v2_refs(doc):
    refs = set()
    for item in _items(doc.get("stale")):
        refs.add(("stale", item.get("path", ""), item.get("target", "")))
    named = doc.get("named") or {}
    for item in _items(named.get("by_path")):
        refs.add(("named", item.get("path", ""), item.get("target", "")))
    for item in _items(named.get("by_key")):
        refs.add(("named_key", item.get("path", ""), item.get("target", "")))
    for item in _items(named.get("collections")):
        refs.add(("collection", item.get("path", ""), item.get("collection", "")))
    return refs


def _v2(doc):
    unmapped, defs = set(), {}
    _unmapped("", doc, V2_CONSUMED, unmapped)
    for d in doc.get("definitions") or []:
        _add_def(defs, _v2_definition(d, unmapped))
    unlisted = _v2_unlisted(doc, unmapped)
    contracts = {(c.get("path", ""), c.get("symbol"), c.get("kind")) for c in _items(doc.get("contracts"))}
    removed = {k for k, d in defs.items() if d["change"] == "removed"}
    for item in _items(doc.get("deleted_modules")):
        removed.add(_node(item))
    return {
        "schema": doc.get("schema"),
        "empty": bool(doc.get("empty")),
        "tests_known": bool((doc.get("tests") or {}).get("known")),
        "defs": defs,
        "unlisted": sorted(unlisted),
        "removed": sorted(removed),
        "refs": sorted(map(list, _v2_refs(doc))),
        "contracts": sorted(map(list, contracts), key=str),
        "run_gaps": sorted(_v2_run_gaps(doc)),
        "unmapped": sorted(unmapped),
    }
