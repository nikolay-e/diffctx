"""The three gold outcomes must not blur into each other.

"Missing from the output" is one symptom of two unrelated failures: discovery
never proposed the file (#130), or it did and the greedy ranked it out (#65).
An attribution that puts a surfaced file in the never-surfaced bucket sends the
next investigation at the wrong subsystem, which is worse than no attribution.
"""

from __future__ import annotations

import json

from eval.analysis.discovery_attribution import attribute, load_dump, render


def _dump(tmp_path, rows):
    p = tmp_path / "prov.jsonl"
    p.write_text("\n".join(json.dumps(r) for r in rows) + "\n")
    return p


def _row(path, selected, source):
    return {"path": path, "selected": selected, "discovery_source": source}


def test_the_three_outcomes_are_separated(tmp_path):
    rows = [
        _row("/repo/a.py", True, "structural"),
        _row("/repo/b.py", False, "lexical_bm25"),
        # c.py is in no row at all — discovery never proposed it.
    ]
    r = attribute(load_dump(_dump(tmp_path, rows)), {"a.py", "b.py", "c.py"})

    assert r["selected"] == ["a.py"]
    assert r["surfaced_not_selected"] == ["b.py"]
    assert r["never_surfaced"] == ["c.py"]


def test_absolute_dump_paths_match_repo_relative_gold(tmp_path):
    """The dump carries absolute paths and the labels are repo-relative. Exact
    equality would drop every gold file into never-surfaced and blame discovery
    for everything."""
    rows = [_row("/tmp/wt/src/deep/mod.py", True, "structural")]
    r = attribute(load_dump(_dump(tmp_path, rows)), {"src/deep/mod.py"})

    assert r["selected"] == ["src/deep/mod.py"]
    assert r["never_surfaced"] == []


def test_a_changed_file_is_reported_as_a_seed_not_a_discovery(tmp_path):
    """`discovery_source` is null for the changed files: they are the seed, not
    something a strategy found. Counting them under a strategy would credit
    discovery with work it did not do."""
    rows = [_row("/repo/changed.py", True, None)]
    r = attribute(load_dump(_dump(tmp_path, rows)), {"changed.py"})

    assert r["surfaced_by_source"] == {"changed-file": 1}


def test_sources_are_counted_per_strategy(tmp_path):
    rows = [
        _row("/repo/a.py", True, "structural"),
        _row("/repo/b.py", True, "structural"),
        _row("/repo/c.py", False, "lexical_bm25"),
    ]
    r = attribute(load_dump(_dump(tmp_path, rows)), {"a.py", "b.py", "c.py"})

    assert r["surfaced_by_source"] == {"structural": 2, "lexical_bm25": 1}


def test_the_readout_names_the_subsystem_for_each_bucket(tmp_path):
    rows = [_row("/repo/a.py", False, "structural")]
    text = render(attribute(load_dump(_dump(tmp_path, rows)), {"a.py", "gone.py"}))

    assert "surfaced, not selected" in text
    assert "never surfaced" in text
    assert "gone.py" in text


class TestProvenanceDumpPathIsPerInstance:
    """`DIFFCTX_PROVENANCE_DUMP` names one file and is read once per run, so a
    cell pointed at a single path keeps only the last instance's rows. Every
    instance needs its own path or the collection is silently a sample of one.
    """

    def test_no_dump_variable_unless_a_directory_is_configured(self):
        from eval.harness.adapters.runner import RunParams

        assert "DIFFCTX_PROVENANCE_DUMP" not in RunParams().to_env("inst-1")

    def test_each_instance_gets_its_own_file(self, tmp_path):
        from eval.harness.adapters.runner import RunParams

        params = RunParams(provenance_dir=str(tmp_path))
        a = params.to_env("inst-1")["DIFFCTX_PROVENANCE_DUMP"]
        b = params.to_env("inst-2")["DIFFCTX_PROVENANCE_DUMP"]

        assert a != b, "two instances would overwrite one dump"
        assert a.endswith("inst-1.jsonl")
        assert b.endswith("inst-2.jsonl")

    def test_a_missing_instance_id_yields_no_dump_rather_than_a_shared_file(self, tmp_path):
        """Falling back to a fixed name would reintroduce the overwrite it is
        meant to prevent, and do it silently."""
        from eval.harness.adapters.runner import RunParams

        env = RunParams(provenance_dir=str(tmp_path)).to_env(None)
        assert "DIFFCTX_PROVENANCE_DUMP" not in env
