"""The union ceiling has to be an oracle, not an average.

`#125`'s readout needs to know how much of the gap between a fusion result and
its two components is reachable by ranking at all. That number is the union of
what each component surfaced — an upper bound no re-ranking of the same two
signals can pass. Getting it wrong in the optimistic direction would make a
failing fusion look like it had headroom left; in the pessimistic direction it
would excuse a real failure.
"""

from __future__ import annotations

import json

from eval.analysis.union_ceiling import ceiling, load_run, render


def _write(tmp_path, name, rows):
    d = tmp_path / name
    d.mkdir()
    (d / "b8000.checkpoint.jsonl").write_text("\n".join(json.dumps(r) for r in rows) + "\n")
    return d


def _row(iid, gold, selected):
    return {"instance_id": iid, "extra": {"gold_files": gold, "selected_files": selected}}


def test_the_union_is_an_upper_bound_on_both_arms(tmp_path):
    """Each arm finds a different half; the union finds all of it."""
    ego = _write(tmp_path, "ego", [_row("i1", ["a.py", "b.py"], ["a.py"])])
    lex = _write(tmp_path, "lex", [_row("i1", ["a.py", "b.py"], ["b.py"])])

    r = ceiling(load_run(ego), load_run(lex), None)["rows"][0]

    assert r["ego"] == 0.5
    assert r["lexical"] == 0.5
    assert r["union"] == 1.0


def test_overlapping_arms_do_not_inflate_the_ceiling(tmp_path):
    """Both arms finding the same file must not count it twice — a set union,
    not a sum. Summing would report 1.0 where the truth is 0.5 and would hide
    every case where the two signals are redundant rather than complementary."""
    ego = _write(tmp_path, "ego", [_row("i1", ["a.py", "b.py"], ["a.py"])])
    lex = _write(tmp_path, "lex", [_row("i1", ["a.py", "b.py"], ["a.py"])])

    r = ceiling(load_run(ego), load_run(lex), None)["rows"][0]

    assert r["union"] == 0.5, "redundant arms must not raise the ceiling"


def test_an_instance_missing_from_one_run_is_skipped(tmp_path):
    """Scoring an instance only one arm ran would compare against a partial
    universe and silently understate the ceiling."""
    ego = _write(tmp_path, "ego", [_row("i1", ["a.py"], ["a.py"]), _row("i2", ["c.py"], ["c.py"])])
    lex = _write(tmp_path, "lex", [_row("i1", ["a.py"], [])])

    assert ceiling(load_run(ego), load_run(lex), None)["instances"] == 1


def test_an_instance_with_no_gold_is_skipped(tmp_path):
    """Recall is undefined with an empty gold set; including it as 0.0 or 1.0
    would move every mean."""
    ego = _write(tmp_path, "ego", [_row("i1", [], ["a.py"])])
    lex = _write(tmp_path, "lex", [_row("i1", [], ["b.py"])])

    assert ceiling(load_run(ego), load_run(lex), None)["instances"] == 0


def test_the_readout_reports_headroom_and_what_fusion_captured(tmp_path):
    ego = _write(tmp_path, "ego", [_row("i1", ["a.py", "b.py"], ["a.py"])])
    lex = _write(tmp_path, "lex", [_row("i1", ["a.py", "b.py"], ["b.py"])])
    fus = _write(tmp_path, "fus", [_row("i1", ["a.py", "b.py"], ["a.py", "b.py"])])

    text = render(ceiling(load_run(ego), load_run(lex), load_run(fus)))

    assert "Headroom over EGO is **+0.500**" in text
    assert "100%" in text, "fusion reaching the ceiling should read as full capture"
