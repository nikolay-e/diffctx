import adjudicate


def row(i, verdict="fn", kind="calls", language="python", **extra):
    return {
        "verdict": verdict, "from": f"p{i}.py::f", "to": "q.py::g", "kind": kind, "site": i, "evidence": None, "repo": "r", "commit": "c",
        "mode": "impact", "language": language, "target_decorated": False, "unresolved": None, **extra,
    }  # fmt: skip


def test_agreement_on_an_easy_call_is_not_hard_and_each_hard_class_is_recognised():
    assert adjudicate.classify(row(1, verdict="tp")) is None
    assert adjudicate.classify(row(1)) == "disagreement"
    assert adjudicate.classify(row(1, kind="overrides", verdict="tp")) == "dynamic_dispatch"
    assert adjudicate.classify(row(1, unresolved="calls through `dyn T` are not traced")) == "dynamic_dispatch"
    assert adjudicate.classify(row(1, target_decorated=True, verdict="tp")) == "framework_registration"
    assert adjudicate.classify(row(1, kind="reexports", verdict="tp")) == "re_export"


def test_rows_of_older_runs_get_their_verdict_from_site_and_evidence():
    # no `verdict` key, and the changed-definition `status` that older rows carried must not be read as one
    legacy = {"status": "modified", "from": "a::f", "to": "b::g", "kind": "calls", "repo": "r", "commit": "c", "mode": "impact"}

    assert adjudicate.classify({**legacy, "site": 3, "evidence": "resolved"}) is None
    assert adjudicate.classify({**legacy, "site": 3, "evidence": None}) == "disagreement"
    assert adjudicate.classify({**legacy, "site": None, "evidence": "resolved"}) == "disagreement"


def test_sample_is_deterministic_balanced_and_stores_references_only():
    rows = [row(i) for i in range(30)] + [row(100 + i, kind="reexports") for i in range(30)]

    first = adjudicate.sample(rows, 20, "seed")
    again = adjudicate.sample(rows, 20, "seed")

    assert first == again
    records = first["python"]
    assert len(records) == 20
    assert {r["class"] for r in records} == {"re_export", "disagreement"}
    assert all(
        set(r) >= {"label_a", "evidence_a", "label_b", "evidence_b", "final", "final_evidence", "sha", "path", "line", "def"}
        for r in records
    )
    assert all(r["final"] is None for r in records)


def test_only_real_disagreements_are_sampled_and_adjudicated_relations_are_not_sampled_again():
    agreeing = [row(i, verdict="tp") for i in range(10)]
    wrong = [row(100 + i, verdict="fp", site=None, evidence="resolved") for i in range(10)]
    done = {("r", "c", row(100 + i)["from"], "q.py::g") for i in range(5)}

    sampled = adjudicate.sample(agreeing + wrong, 100, "seed", done)["python"]

    assert {r["from"] for r in sampled} == {row(100 + i)["from"] for i in range(5, 10)}
    assert all(r["scip_kind"] is None and r["observed"] == {"scip": False, "diffctx": True, "verdict": "fp"} for r in sampled)
