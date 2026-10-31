import adjudicate


def row(i, status="fn", kind="calls", language="python", **extra):
    return {
        "status": status, "from": f"p{i}.py::f", "to": "q.py::g", "kind": kind, "site": i, "evidence": None, "repo": "r", "commit": "c",
        "mode": "impact", "language": language, "target_decorated": False, "unresolved": None, **extra,
    }  # fmt: skip


def test_agreement_on_an_easy_call_is_not_hard_and_each_hard_class_is_recognised():
    assert adjudicate.classify(row(1, status="tp")) is None
    assert adjudicate.classify(row(1)) == "disagreement"
    assert adjudicate.classify(row(1, kind="overrides", status="tp")) == "dynamic_dispatch"
    assert adjudicate.classify(row(1, unresolved="calls through `dyn T` are not traced")) == "dynamic_dispatch"
    assert adjudicate.classify(row(1, target_decorated=True, status="tp")) == "framework_registration"
    assert adjudicate.classify(row(1, kind="reexports", status="tp")) == "re_export"


def test_sample_is_deterministic_balanced_and_stores_references_only():
    rows = [row(i) for i in range(30)] + [row(100 + i, kind="reexports") for i in range(30)]

    first = adjudicate.sample(rows, 20, "seed")
    again = adjudicate.sample(rows, 20, "seed")

    assert first == again
    records = first["python"]
    assert len(records) == 20
    assert {r["class"] for r in records} == {"re_export", "disagreement"}
    assert all(
        set(r) >= {"label_a", "evidence_a", "label_b", "evidence_b", "final", "final_evidence", "sha", "path", "line"}
        for r in records
    )
    assert all(r["final"] is None for r in records)
