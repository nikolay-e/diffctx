"""The case's own expectations, read against a normalized answer.

Ports the matching of tests/common/oracle/{model,judge}.rs that the hedge
comparison needs: Node.covers, relation pairing (resolution, sites, from),
exactness per (target, family), and the coverage / unknowns expectations.
Not ported: text-form checks, lists (stale, contracts, untested, ...), decoys.
"""

import re

CODE_KINDS = {
    "calls",
    "reads",
    "writes",
    "imports",
    "reexports",
    "overrides",
    "instantiates",
    "passes_as_value",
    "type_use",
    "configures",
    "executes",
    "references_path",
}


def is_identifier(text):
    return bool(re.fullmatch(r"[^\W\d][\w$]*|\$[\w$]*", text))


class Node:
    __slots__ = ("path", "symbol")

    def __init__(self, path, symbol=None):
        self.path, self.symbol = path, symbol

    @classmethod
    def parse(cls, text):
        if "::" in text:
            path, symbol = text.split("::", 1)
            return cls(path, symbol)
        return cls(text, None)

    @classmethod
    def target(cls, text):
        if "::" not in text and is_identifier(text):
            return cls("", text)
        return cls.parse(text)

    def key(self):
        return (self.path, self.symbol)

    def __hash__(self):
        return hash(self.key())

    def __eq__(self, other):
        return self.key() == other.key()

    def __str__(self):
        if self.symbol is None:
            return self.path
        return self.symbol if not self.path else f"{self.path}::{self.symbol}"

    def covers(self, found):
        if self.path and self.path != found.path:
            return False
        if self.symbol is None:
            return True
        have = found.symbol
        if have is None:
            return False
        want = self.symbol
        if have == want:
            return True

        def last(s):
            return re.split(r"[.:]", s)[-1]

        def sep(s):
            return bool(re.search(r"[.:]", s))

        return (not sep(have) or not sep(want)) and last(want) == last(have)


class Rel:
    """One relation: from -> to, with resolution, sites and the family."""

    __slots__ = ("frm", "kind", "loose", "resolution", "sites", "stated", "to")

    def __init__(self, frm, to, kind, resolution, sites, loose=False, stated=False):
        self.frm, self.to, self.kind = frm, to, kind
        self.resolution, self.sites = resolution, list(sites)
        self.loose, self.stated = loose, stated

    @property
    def family(self):
        return "tests" if self.kind == "tests_reach" else "code"

    def show(self):
        lines = f", line {'+'.join(map(str, self.sites))}" if self.sites else ""
        return f"{self.frm} -{self.kind}-> {self.to} [{self.resolution}{lines}]"


def resolution_of(standing):
    return {
        "resolved": "resolved",
        "reexport": "resolved",
        "candidate": "candidate",
    }.get(standing, "unresolved")


def kind_of(cls):
    return {
        "reference": "reads",
        "implementation": "overrides",
        "reexports": "reexports",
    }.get(cls, "calls")


def answer_relations(norm):
    """(relations, folded) the way engine.rs::extract builds them. A row folded by
    the `untyped-receiver` rule stays a relation: the text says "possible"."""
    rels, folded = [], []
    for d in norm["defs"].values():
        to = Node(d["path"], d["symbol"])
        for row in d["rows"]:
            standing = row["standing"]
            cls = row["cls"]
            evidence = "reexport" if cls == "reexports" else standing
            if standing == "name":
                continue
            frm = Node(row["path"], row["symbol"])
            rel = Rel(frm, to, kind_of(cls), resolution_of(evidence), row["sites"])
            fb = row["folded_by"]
            if fb and fb != "untyped-receiver":
                folded.append((rel, fb))
                continue
            rels.append(rel)
            for test, name in row["tests"]:
                rels.append(Rel(Node(test or "", name), frm, "tests_reach", "resolved", []))
        for path in d["missed_in"]:
            rels.append(Rel(Node(path), to, "calls", "unresolved", []))
    return rels, folded


def compile_expect(case):
    expect = case.get("expect") or {}
    rels, listed = [], set()
    for spec in expect.get("relations") or []:
        to = Node.parse(spec["to"])
        fam = "tests" if spec["kind"] == "tests_reach" else "code"
        listed.add((to, fam))
        site = spec.get("site")
        rels.append(
            Rel(
                Node.parse(spec["from"]),
                to,
                spec["kind"],
                spec.get("resolution", "resolved"),
                [site] if site else [],
            )
        )
    for target, froms in (expect.get("callers") or {}).items():
        to = Node.target(target)
        listed.add((to, "code"))
        for frm in froms:
            rels.append(Rel(Node.parse(frm), to, "calls", "resolved", [], loose=True))
    for file, tests in (expect.get("tested_by") or {}).items():
        to = Node.parse(file)
        listed.add((to, "tests"))
        for t in ([tests] if isinstance(tests, str) else tests):
            rels.append(Rel(Node.parse(t), to, "tests_reach", "resolved", []))
    return rels, listed


def pairs(want, got):
    tests = want.kind == "tests_reach"
    from_ok = want.frm.covers(got.frm) or (
        got.kind == "references_path" and got.frm.symbol is None and want.frm.path == got.frm.path
    )
    sites_ok = (tests and not got.sites) or all(s in got.sites for s in want.sites)
    return from_ok and (want.kind == "reexports") == (got.kind == "reexports") and want.resolution == got.resolution and sites_ok


def folded_stands_for(want, folded):
    rel, rule = folded
    kinds = {
        "declared-type": {"type_use"},
        "override-not-run": {"overrides"},
        "imports-a-body": {"imports", "reexports"},
    }.get(rule, set())
    if want.loose or want.kind not in kinds:
        return False
    as_wanted = Rel(rel.frm, rel.to, want.kind, rel.resolution, rel.sites)
    return want.to.covers(rel.to) and pairs(want, as_wanted)


def target_exact(case, wants, listed, rels, folded, target, fam):
    """True when the answer's relations to `target` in `fam` are exactly the
    expected ones (extra rows count when `exact`, the default)."""
    exact = (case.get("expect") or {}).get("exact", True)
    expected = [w for w in wants if w.to == target and w.family == fam]
    actual = [a for a in rels if target.covers(a.to) and a.family == fam]
    taken = [False] * len(actual)
    for want in expected:
        hits = 0
        for i, got in enumerate(actual):
            if (taken[i] and not want.sites) or not pairs(want, got):
                continue
            taken[i] = True
            hits += 1
            if want.frm.symbol is not None:
                break
        if hits == 0 and any(folded_stands_for(want, f) for f in folded):
            hits = 1
        if hits == 0:
            return False
    if exact and not all(taken):
        return False
    return True


class Verdicts:
    """What the case says about one binary's answer, per target and per row."""

    def __init__(self, case, norm):
        self.case, self.norm = case, norm
        self.wants, self.listed = compile_expect(case)
        self.rels, self.folded = answer_relations(norm)
        exp = case.get("expect") or {}
        self.coverage = {Node.target(k): v for k, v in (exp.get("coverage") or {}).items()}
        self.unknowns = None if exp.get("unknowns") is None else {Node.target(k): v for k, v in exp["unknowns"].items()}

    def targets_for(self, d):
        node = Node(d["path"], d["symbol"])
        return [(t, f) for (t, f) in self.listed if t.covers(node)]

    def def_judged(self, d):
        return bool(self.targets_for(d)) or self._cov_keys(d) or self._unk_keys(d)

    def _cov_keys(self, d):
        node = Node(d["path"], d["symbol"])
        return [k for k in self.coverage if k.covers(node)]

    def _unk_keys(self, d):
        if self.unknowns is None:
            return []
        node = Node(d["path"], d["symbol"])
        return [k for k in self.unknowns if k.covers(node)]

    def relations_ok(self, d):
        """None when no relation expectation names the definition."""
        pairs_ = self.targets_for(d)
        if not pairs_:
            return None
        return all(target_exact(self.case, self.wants, self.listed, self.rels, self.folded, t, f) for t, f in pairs_)

    def coverage_ok(self, d, answer_partial):
        """None when the case states no coverage for the definition."""
        keys = self._cov_keys(d)
        if not keys:
            return None
        for key in keys:
            for kind, expected in self.coverage[key].items():
                found = "partial" if (answer_partial or (kind == "tests_reach" and not self.norm["tests_known"])) else "complete"
                if found != expected:
                    return False
        return True

    def expects_unknown(self, d):
        return bool(self._unk_keys(d))

    def expects_partial(self, d):
        for key in self._cov_keys(d):
            if "partial" in self.coverage[key].values():
                return True
        return self.expects_unknown(d)

    def row_expected(self, d, row):
        """(listed, matches): whether the case lists the definition's relations,
        and whether `row` (as this answer states it) matches an expected one."""
        node = Node(d["path"], d["symbol"])
        wants = [
            w
            for w in self.wants
            if w.family == "code" and w.to.covers(node) and any(t == w.to and f == "code" for t, f in self.listed)
        ]
        listed = any(f == "code" and t.covers(node) for t, f in self.listed)
        if not listed:
            return False, None
        rel = Rel(
            Node(row["path"], row["symbol"]),
            node,
            kind_of(row["cls"]),
            resolution_of("reexport" if row["cls"] == "reexports" else row["standing"]),
            row["sites"],
        )
        return True, any(pairs(w, rel) for w in wants)


def _listed_nodes(expect, key):
    return [Node.target(x) for x in (expect.get(key) or [])]


def presence_ok(verdicts, node, present):
    """Whether the case's `changed` (impact) or `definitions` (symbol) list agrees
    with the definition being named or not; None when the case lists none."""
    expect = verdicts.case.get("expect") or {}
    key = "changed" if verdicts.case.get("mode") == "impact" else "definitions"
    if expect.get(key) is None:
        return None

    def names(w, a):  # the harness's compare_set: a bare path names the file's own row
        if w.symbol is None and w.path:
            return w.path == a.path and a.symbol is None
        return w.covers(a)

    wanted = any(names(w, node) for w in _listed_nodes(expect, key))
    if present and not wanted:
        return not expect.get("exact", True)  # only an exact list objects to an extra name
    return present == wanted or (present and wanted)


def contract_ok(verdicts, path, present):
    expect = verdicts.case.get("expect") or {}
    if path in (expect.get("contracts") or []):
        return present
    if path in (expect.get("not_contracts") or []):
        return not present
    return None


def ref_ok(verdicts, kind, path, present):
    expect = verdicts.case.get("expect") or {}
    yes, no = ("stale", "not_stale") if kind == "stale" else ("named_by", "not_named_by")
    if path in (expect.get(yes) or []):
        return present
    if path in (expect.get(no) or []):
        return not present
    return None


def _names(w, a):
    if w.symbol is None and w.path:
        return w.path == a.path and a.symbol is None
    return w.covers(a)


def _list_red(v, expect):
    """The case's `changed` (impact) or `definitions` (symbol) list against the answer's."""
    key = "changed" if v.case.get("mode") == "impact" else "definitions"
    if expect.get(key) is None:
        return False
    n = v.norm
    touched = [Node(d["path"], d["symbol"]) for d in n["defs"].values()]
    touched += [Node.parse(u) for u in n["unlisted"]]
    wanted = _listed_nodes(expect, key)
    if any(not any(_names(w, a) for a in touched) for w in wanted):
        return True
    return expect.get("exact", True) and any(not any(_names(w, a) for w in wanted) for a in touched)


def _coverage_red(v):
    n = v.norm
    for key_node, kinds in v.coverage.items():
        defs = [d for d in n["defs"].values() if key_node.covers(node_of(d))]
        if not defs:
            return True
        partial = any(d["gaps"] or d["missed_in"] for d in defs) or bool(n["run_gaps"])
        for kind, want in kinds.items():
            found = "partial" if (partial or (kind == "tests_reach" and not n["tests_known"])) else "complete"
            if found != want:
                return True
    return False


def _unknowns_red(v):
    if v.unknowns is None:
        return False
    gapped = [(node_of(d), g["cls"]) for d in v.norm["defs"].values() for g in d["gaps"]]
    if not v.unknowns and gapped:
        return True
    return any(not any(key_node.covers(nd) and c == cls for nd, c in gapped) for key_node, cls in v.unknowns.items())


def case_red(verdicts):
    """A partial port of the oracle's verdict: relations per target, the changed
    or definitions list, coverage and unknowns. Text checks, decoys and the
    stale/named/contract/untested lists are not ported, so a red the oracle
    finds only there reads green here."""
    v, n = verdicts, verdicts.norm
    expect = v.case.get("expect") or {}
    if any(not target_exact(v.case, v.wants, v.listed, v.rels, v.folded, t, f) for t, f in v.listed):
        return True
    if _list_red(v, expect) or _coverage_red(v) or _unknowns_red(v):
        return True
    if expect.get("never_empty") and n["empty"]:
        return True
    return bool(expect.get("discloses") and not (n["run_gaps"] or any(d["gaps"] or d["missed_in"] for d in n["defs"].values())))


def node_of(d):
    return Node(d["path"], d["symbol"])
