"""SWE-Explore normalisation, and the two places it must refuse rather than guess.

The corpus is worth having because its gold is trajectory-derived — what
successful agent runs actually read — rather than patch-derived. That only holds
if the adapter maps the right field: reading `modified_core_files` as gold would
turn a retrieval corpus into another retention corpus and nothing downstream
would notice.

Fixtures are hand-written to the documented schema, not copied from the dataset:
it is CC-BY-NC-ND, so no content from it enters this repository.
"""

from __future__ import annotations

from eval.harness.adapters.swe_explore import SweExploreAdapter, parse_regions

PATCH = """diff --git a/src/core.py b/src/core.py
--- a/src/core.py
+++ b/src/core.py
@@ -1,3 +1,4 @@
 def run():
+    validate()
     return 1
"""


def _row(**over):
    row = {
        "instance_id": "acme__widget-42",
        "patch": PATCH,
        "meta": {"repo": "acme/widget", "base_commit": "a" * 40},
        "ground_truth": {
            "read_core_files": ["src/core.py", "src/validate.py"],
            "read_core_regions": [
                {"path": "src/core.py", "start": 1, "end": 4},
                {"path": "src/validate.py", "start": 10, "end": 25},
            ],
            "modified_core_files": ["src/core.py"],
            "read_optional_regions_map": {"model-a": [], "model-b": []},
        },
    }
    row.update(over)
    return row


def _norm(**over):
    return SweExploreAdapter()._normalize(_row(**over))


class TestGoldComesFromTrajectoriesNotThePatch:
    def test_regions_read_by_trajectories_become_line_scoped_gold(self):
        inst = _norm()
        assert inst is not None
        spans = {(f.path, f.start_line, f.end_line) for f in inst.gold_fragments or ()}
        assert spans == {("src/core.py", 1, 4), ("src/validate.py", 10, 25)}
        assert all(f.kind == "region" for f in inst.gold_fragments or ())

    def test_a_file_read_but_never_modified_is_still_gold(self):
        """`src/validate.py` is what the corpus exists to measure: a file the
        patch never touched that solving the issue required reading. Scoring
        only modified files would make this corpus redundant."""
        inst = _norm()
        assert inst is not None
        assert "src/validate.py" in inst.gold_files
        assert inst.extra["nontrivial_gold"] == ["src/validate.py"]
        assert inst.extra["nontrivial_gold_count"] == 1

    def test_optional_regions_are_recorded_but_not_scored(self):
        """Optional regions are one model's detours. Treating them as gold would
        penalise a tool for not reproducing another model's wandering."""
        inst = _norm()
        assert inst is not None
        assert inst.extra["optional_region_models"] == ["model-a", "model-b"]
        assert "src/optional.py" not in inst.gold_files


class TestItRefusesRatherThanInvents:
    def test_an_instance_without_a_patch_is_skipped(self):
        """diffctx is diff-seeded. Synthesising a patch from
        `modified_core_files` would be the adapter inventing the input and
        calling the output a measurement."""
        assert _norm(patch="") is None
        assert _norm(patch="   ") is None

    def test_an_instance_without_trajectory_gold_is_skipped(self):
        assert _norm(ground_truth={"read_core_files": [], "read_core_regions": []}) is None
        assert _norm(ground_truth="not a dict") is None

    def test_an_instance_without_an_id_is_skipped(self):
        """A missing id would collide with every other unnamed instance once the
        harness keys results by it."""
        assert _norm(instance_id=None) is None


class TestSeedingProvenanceSurvives:
    def test_the_row_records_that_the_input_differs_from_the_benchmark(self):
        """These numbers are not comparable to published SWE-Explore results:
        the benchmark seeds from the issue, this seeds from the patch. The
        distinction has to travel with the row or an analysis will lose it."""
        inst = _norm()
        assert inst is not None
        assert inst.extra["seeding"] == "diff"
        assert inst.extra["benchmark_seeding"] == "issue"
        assert inst.extra["gold_provenance"] == "agent_trajectories"

    def test_the_licence_travels_with_every_instance(self):
        inst = _norm()
        assert inst is not None
        assert inst.extra["dataset_license"] == "CC-BY-NC-ND-4.0"

    def test_the_instance_id_is_namespaced(self):
        inst = _norm()
        assert inst is not None
        assert inst.instance_id == "swe_explore::acme__widget-42"
        assert inst.source_benchmark == "swe_explore"


class TestRegionParsing:
    def test_a_region_with_no_usable_bounds_degrades_to_the_whole_file(self):
        """Dropping it would understate recall while looking like the tool
        missed nothing — the file is still genuinely gold."""
        frags = parse_regions([{"path": "a.py"}, {"path": "b.py", "start": 9, "end": 3}])
        assert [(f.path, f.kind) for f in frags] == [("a.py", "file"), ("b.py", "file")]

    def test_malformed_entries_are_ignored_not_fatal(self):
        assert parse_regions("not a list") == ()
        assert parse_regions([None, 42, {"no_path": 1}, {"path": ""}]) == ()
