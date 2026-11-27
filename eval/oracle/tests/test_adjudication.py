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
            "reported": True,
            "fcn": False,
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


def raw_cell(frm_kind="calls", **extra):
    return {
        "repo": "r", "commit": "c", "mode": "impact", "target": "q.py::g", "kind": frm_kind, "kind_mismatches": 0,
        "abstained": False, "reported": True, "fcn": False, **extra,
    }  # fmt: skip


def test_a_target_the_answer_reported_and_listed_nothing_for_is_a_false_complete_negative_after_adjudication():
    cells = adj.cells_from_rows([row("a", "fn", affected=True)], [raw_cell(fcn=True)])

    assert [(c["truth"], c["fcn"]) for c in cells] == [(1, True)]


def test_a_language_with_no_adjudicated_verdict_keeps_the_raw_false_complete_negatives():
    # The answer had no entry for the target (it found no definition of it) and truth lists a caller: the raw cell is no FCN.
    # Rebuilding the table from rows alone used to turn every such target into one; it must not, verdicts or none.
    silent = [row("a", "fn", affected=True, to="q.py::unreported")]
    folded = [row("b", "fn", affected=False, to="q.py::cannot_reach")]
    raw = [
        raw_cell(target="q.py::unreported", reported=False),
        raw_cell(target="q.py::cannot_reach"),
    ]

    cells = adj.cells_from_rows(adj.apply_to_rows([*silent, *folded], {}), raw)

    assert [(c["target"], c["truth"], c["fcn"]) for c in cells] == [
        ("q.py::unreported", 1, False),
        ("q.py::cannot_reach", 1, False),
    ]


def test_an_abstaining_answer_is_no_false_complete_negative_after_adjudication_either():
    cells = adj.cells_from_rows([row("a", "fn", affected=True)], [raw_cell(abstained=True)])

    assert [(c["fcn"], c["abstained"]) for c in cells] == [(False, True)]


def test_the_affected_and_folded_columns_survive_the_rebuild_so_the_adjudicated_table_reads_like_the_raw_one():
    rows = [
        row("a", "tp", affected=True),
        row("b", "tp", affected=False, folded_by="check-the-default"),
        row("c", "tp", affected=True, folded_by="imports-a-body", false_fold=True),
        row("d", "fn", affected=False),
    ]

    (cell,) = adj.cells_from_rows(rows, [raw_cell()])

    assert (cell["truth"], cell["truth_affected"], cell["tp_affected"], cell["folded"], cell["false_fold"]) == (4, 2, 1, 2, 1)


def test_the_rebuilt_cell_counts_the_incomplete_complete_and_the_false_complete_negative_as_the_raw_cell_does():
    rows = [row("a", "tp", affected=True), row("b", "fn", affected=True), row("c", "fn", affected=False)]
    listed = [raw_cell(listed=True, reported=True)]

    [cell] = adj.cells_from_rows(rows, listed)

    assert (cell["incomplete_complete"], cell["fcn"]) == (
        1,
        False,
    )  # one affected truth site unlisted, but the answer listed others
    [silent] = adj.cells_from_rows([row("b", "fn", affected=True)], [raw_cell(listed=False, reported=True)])
    assert (silent["incomplete_complete"], silent["fcn"]) == (1, True)
    [no_entry] = adj.cells_from_rows([row("b", "fn", affected=True)], [raw_cell(reported=False)])
    assert (no_entry["incomplete_complete"], no_entry["fcn"]) == (0, False)


def test_an_adjudication_that_refutes_the_unlisted_site_takes_it_out_of_the_incomplete_complete():
    rows = [row("a", "fn", affected=True), row("b", "fn", affected=True)]
    verdicts = {key("a"): verdict("a", "none")}

    [cell] = adj.cells_from_rows(adj.apply_to_rows(rows, verdicts), [raw_cell(reported=True)])

    assert (cell["truth"], cell["incomplete_complete"]) == (1, 1)


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


def test_a_type_use_and_an_import_are_labels_an_adjudicator_may_give():
    assert adj.KIND_OF["type_use"] == "type_use" and adj.KIND_OF["import"] == "imports"
    assert {"type_use", "import"} <= adj.DECIDED


def test_a_gap_in_one_search_group_abstains_that_groups_cells_only_after_adjudication_too():
    raw = [raw_cell("calls"), raw_cell("reads", abstained=True)]
    rows = [row("a", "fn", kind="calls", affected=True), row("b", "fn", kind="reads", affected=True)]

    cells = {c["kind"]: c for c in adj.cells_from_rows(rows, raw)}

    assert (cells["calls"]["incomplete_complete"], cells["reads"]["incomplete_complete"]) == (1, 0)
