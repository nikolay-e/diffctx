from __future__ import annotations

import json
from pathlib import Path

import pytest

# CI's test job installs no eval group; the analysis module needs numpy.
pytest.importorskip("numpy")

from eval.analysis.stratified_analysis import load_long

# `load_long` shipped with `rows.append(row)` under a `continue` and `return`
# inside the loop: every cell-* directory after the first was ignored and the
# rows of the first were never appended. Nothing exercised it, so a green suite
# said nothing. Two cells, both layouts, every row must come back.


def _cell(root: Path, name: str, method: str, budget: int, multi: bool, rows: list[dict]) -> None:
    cell = root / f"cell-{name}"
    cell.mkdir(parents=True)
    (cell / "metadata.json").write_text(
        json.dumps({"cell": {"method": method, "budget": budget, "test_set": "contextbench_verified", "depth": 2}})
    )
    if multi:
        target = cell / "contextbench_verified_budget_sweep" / "L2" / f"b{budget}.checkpoint.jsonl"
    else:
        target = cell / "contextbench_verified.checkpoint.jsonl"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("".join(json.dumps(r) + "\n" for r in rows))


def _row(iid: str, recall: float) -> dict:
    return {
        "instance_id": iid,
        "file_recall": recall,
        "file_precision": 0.5,
        "used_tokens": 1000,
        "source_benchmark": "contextbench_verified",
        "extra": {"status": "ok", "language": "python", "n_gold": 2, "n_nontrivial_gold": 1},
    }


def test_every_row_of_every_cell_and_both_layouts_comes_back(tmp_path):
    _cell(tmp_path, "ego-flat", "ego", 8000, False, [_row("a", 1.0), _row("b", 0.5)])
    _cell(tmp_path, "bm25-multi", "internal-bm25", 16000, True, [_row("a", 0.0), _row("c", 1.0), _row("d", 0.25)])

    rows = load_long(tmp_path)

    assert len(rows) == 5, [(r.get("method"), r.get("instance_id")) for r in rows]
    by_method = {}
    for r in rows:
        by_method.setdefault(r["method"], set()).add(r["instance_id"])
    assert by_method == {"ego": {"a", "b"}, "internal-bm25": {"a", "c", "d"}}
    assert {r["budget"] for r in rows if r["method"] == "internal-bm25"} == {16000}
