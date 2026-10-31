import json
from pathlib import Path

import adjudication as adj


def row(frm, verdict=None, kind="calls", site=3, evidence="resolved", to="q.py::g", **extra):
    base = {
        "repo": "r", "commit": "c", "language": "python", "mode": "impact", "from": frm, "to": to, "kind": kind, "site": site,
        "evidence": evidence,
    }  # fmt: skip
    return {**base, **({"verdict": verdict} if verdict else {}), **extra}


def verdict(frm, final, scip_kind="calls", to="q.py::g"):
    return {"repo": "r", "sha": "c", "from": frm, "def": to, "final": final, "scip_kind": scip_kind}


def key(frm, to="q.py::g"):
    return ("r", "c", frm, to)


def test_older_rows_get_their_verdict_from_site_and_evidence():
    assert adj.verdict_of(row("a", site=3, evidence="resolved")) == "tp"
    assert adj.verdict_of(row("a", site=3, evidence="candidate")) == "tp_weak"
    assert adj.verdict_of(row("a", site=3, evidence=None)) == "fn"
    assert adj.verdict_of(row("a", site=None, evidence="resolved")) == "fp"
    assert adj.verdict_of(row("a", site=None, evidence="name")) == "fp_weak"
    assert adj.verdict_of(row("a", verdict="tp", site=None)) == "tp"


def test_none_refutes_a_scip_relation_a_kind_relabels_it_and_unresolved_drops_it():
    rows = [
        row("a", "tp"),
        row("b", "fn"),
        row("c", "tp"),
        row("d", "fp", site=None),
        row("e", "fn"),
        row("f", "tp"),
        row("g", "tp"),
    ]
    verdicts = {
        key("a"): verdict("a", "none"),  # diffctx claimed it, SCIP had it, the evidence says neither: now a false positive
        key("b"): verdict("b", "none"),  # SCIP had it and diffctx missed it: nothing was missed
        key("c"): verdict("c", "instantiate"),  # a relabel keeps the outcome
        key("d"): verdict("d", "call"),  # SCIP lacked it, diffctx was right
        key("e"): verdict("e", "unresolved"),
    }

    out = adj.apply_to_rows(rows, verdicts)

    assert [(r["from"], adj.verdict_of(r), r["kind"]) for r in out] == [
        ("a", "fp", "calls"),
        ("c", "tp", "instantiates"),
        ("d", "tp", "calls"),
        ("f", "tp", "calls"),
        ("g", "tp", "calls"),
    ]


def test_cells_are_rebuilt_from_rows_keeping_the_answers_abstention():
    rows = [row("a", "tp"), row("b", "fn"), row("c", "fp", kind="reads", site=None)]
    raw = [
        {
            "repo": "r",
            "commit": "c",
            "mode": "impact",
            "target": "q.py::g",
            "kind": "calls",
            "kind_mismatches": 2,
            "abstained": True,
        }
    ]

    cells = {c["kind"]: c for c in adj.cells_from_rows(rows, raw)}

    assert (cells["calls"]["truth"], cells["calls"]["tp"], cells["calls"]["fn"], cells["calls"]["kind_mismatches"]) == (
        2,
        1,
        1,
        2,
    )
    assert (cells["reads"]["truth"], cells["reads"]["fp"], cells["reads"]["abstained"]) == (0, 1, True)
    assert not any(c["fcn"] for c in cells.values())


def test_a_target_nothing_reported_and_nothing_explains_is_a_false_complete_negative_after_adjudication():
    cells = adj.cells_from_rows([row("a", "fn")], [])

    assert [(c["truth"], c["fcn"]) for c in cells] == [(1, True)]


def test_load_verdicts_reads_final_sets_only_and_summary_counts_outcomes(tmp_path: Path):
    records = [
        {**verdict("a", "call"), "scip_kind": "calls", "sha": "git:c"},
        {**verdict("b", "read"), "scip_kind": "calls"},
        {**verdict("c", "none"), "scip_kind": "calls"},
        {**verdict("d", "unresolved"), "scip_kind": "reads"},
        {**verdict("e", None), "scip_kind": "reads"},
    ]
    (tmp_path / "python.jsonl").write_text("".join(json.dumps(r) + "\n" for r in records))
    (tmp_path / "python.A.jsonl").write_text(json.dumps(verdict("z", "call")) + "\n")

    loaded = adj.load_verdicts(tmp_path)

    assert {k[2] for k in loaded} == {"a", "b", "c", "d"}
    assert {k[1] for k in loaded} == {"c"}
    assert adj.summary(loaded, {"r": "python"}) == [
        {"language": "python", "total": 4, "confirmed": 1, "relabelled": 1, "refuted": 1, "unresolved": 1}
    ]


def test_the_shipped_adjudicated_sets_are_decided_references():
    from corpus import ROOT

    loaded = adj.load_verdicts(ROOT / "adjudicated")

    assert len(loaded) == 196
    assert all(r["final"] in adj.DECIDED and r["scip_kind"] for r in loaded.values())
    assert not any("code" in r for r in loaded.values())
