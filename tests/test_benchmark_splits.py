from __future__ import annotations

from collections.abc import Iterator
from pathlib import Path

from benchmarks.adapters import BenchmarkInstance
from benchmarks.adapters.base import BenchmarkAdapter
from benchmarks.adapters.splits import (
    SplitConfig,
    build_splits,
    render_split_report,
    write_manifests,
)


class _StubAdapter(BenchmarkAdapter):
    def __init__(self, name: str, instances: list[BenchmarkInstance]) -> None:
        self.name = name
        self._instances = instances

    def dataset_revision(self) -> str:
        return f"stub://{self.name}@frozen"

    def _load_raw(self) -> Iterator[dict]:
        return iter(())

    def _normalize(self, row: dict) -> BenchmarkInstance | None:
        return None

    def load(self) -> Iterator[BenchmarkInstance]:
        yield from self._instances


def _make(
    source: str,
    idx: int,
    language: str = "python",
    repo: str | None = None,
    sha: str | None = None,
) -> BenchmarkInstance:
    return BenchmarkInstance(
        instance_id=f"{source}::{idx}",
        source_benchmark=source,
        repo=repo or f"owner/{source}-{idx}",
        base_commit=sha or f"{idx:040x}",
        gold_patch="",
        gold_files=frozenset({"f.py"}),
        language=language,
    )


def test_test_set_is_full_passthrough_of_test_adapters():
    test_adapter = _StubAdapter("test_a", [_make("test_a", i) for i in range(5)])
    pool_adapter = _StubAdapter("pool_b", [_make("pool_b", i) for i in range(20)])
    config = SplitConfig(
        test_only_adapters=(test_adapter,),
        calibration_pool_adapters=(pool_adapter,),
        validation_fraction=0.0,
    )
    result = build_splits(config)
    assert result.stats.test_total == 5
    assert result.stats.test_per_benchmark == {"test_a": 5}
    assert "test_a" in result.test_ids
    assert len(result.test_ids["test_a"]) == 5


def test_contamination_filter_drops_pool_twins_of_test_instances():
    shared_repo, shared_sha = "owner/shared", "deadbeef" * 5
    test_inst = _make("test_a", 1, repo=shared_repo, sha=shared_sha)
    twin_inst = _make("pool_b", 1, repo=shared_repo, sha=shared_sha)  # same repo+sha
    safe_inst = _make("pool_b", 2)
    config = SplitConfig(
        test_only_adapters=(_StubAdapter("test_a", [test_inst]),),
        calibration_pool_adapters=(_StubAdapter("pool_b", [twin_inst, safe_inst]),),
        validation_fraction=0.0,
    )
    result = build_splits(config)
    assert "pool_b::1" not in result.calibration_ids  # twin dropped
    assert "pool_b::2" in result.calibration_ids
    assert result.stats.pool_before_dedup == 2
    assert result.stats.pool_dropped_by_contamination == 1


def test_validation_fraction_carves_holdout_per_stratum():
    instances = [_make("pool", i, language="python") for i in range(20)] + [
        _make("pool", 100 + i, language="java") for i in range(10)
    ]
    config = SplitConfig(
        test_only_adapters=(),
        calibration_pool_adapters=(_StubAdapter("pool", instances),),
        validation_fraction=0.20,
    )
    result = build_splits(config)
    # Per-stratum: 20 python → 16 cal + 4 val; 10 java → 8 cal + 2 val
    assert result.stats.per_stratum[("pool", "python")] == (16, 4)
    assert result.stats.per_stratum[("pool", "java")] == (8, 2)
    assert result.stats.calibration_total == 24
    assert result.stats.validation_total == 6


def test_split_is_deterministic_under_fixed_seed():
    instances = [_make("pool", i) for i in range(50)]
    config_a = SplitConfig(
        test_only_adapters=(),
        calibration_pool_adapters=(_StubAdapter("pool", instances),),
        validation_fraction=0.20,
        seed=42,
    )
    config_b = SplitConfig(
        test_only_adapters=(),
        calibration_pool_adapters=(_StubAdapter("pool", list(instances)),),
        validation_fraction=0.20,
        seed=42,
    )
    a = build_splits(config_a)
    b = build_splits(config_b)
    assert a.calibration_ids == b.calibration_ids
    assert a.validation_ids == b.validation_ids


def test_different_seeds_produce_different_validation_sets():
    instances = [_make("pool", i) for i in range(50)]
    a = build_splits(
        SplitConfig(
            test_only_adapters=(),
            calibration_pool_adapters=(_StubAdapter("pool", instances),),
            validation_fraction=0.20,
            seed=1,
        )
    )
    b = build_splits(
        SplitConfig(
            test_only_adapters=(),
            calibration_pool_adapters=(_StubAdapter("pool", list(instances)),),
            validation_fraction=0.20,
            seed=2,
        )
    )
    assert a.validation_ids != b.validation_ids


def test_test_calibration_validation_sets_are_disjoint():
    test_a = _make("test", 1, repo="owner/x", sha="a" * 40)
    pool = [_make("pool", i) for i in range(30)]
    config = SplitConfig(
        test_only_adapters=(_StubAdapter("test", [test_a]),),
        calibration_pool_adapters=(_StubAdapter("pool", pool),),
        validation_fraction=0.20,
    )
    result = build_splits(config)
    assert result.calibration_ids & result.validation_ids == set()
    test_global = set().union(*result.test_ids.values())
    assert test_global & result.calibration_ids == set()
    assert test_global & result.validation_ids == set()


def test_dataset_revisions_recorded_per_adapter():
    config = SplitConfig(
        test_only_adapters=(_StubAdapter("ta", []),),
        calibration_pool_adapters=(_StubAdapter("pb", []),),
        validation_fraction=0.10,
    )
    result = build_splits(config)
    assert result.stats.dataset_revisions["ta"] == "stub://ta@frozen"
    assert result.stats.dataset_revisions["pb"] == "stub://pb@frozen"


def test_write_manifests_emits_sorted_text_files(tmp_path: Path):
    test_a = _StubAdapter("test_a", [_make("test_a", i) for i in range(3)])
    pool = _StubAdapter("pool", [_make("pool", i) for i in range(5)])
    result = build_splits(
        SplitConfig(
            test_only_adapters=(test_a,),
            calibration_pool_adapters=(pool,),
            validation_fraction=0.20,
        )
    )
    write_manifests(result, tmp_path)
    assert (tmp_path / "test_test_a.txt").exists()
    assert (tmp_path / "calibration.txt").exists()
    assert (tmp_path / "validation.txt").exists()
    test_lines = (tmp_path / "test_test_a.txt").read_text().strip().splitlines()
    assert test_lines == sorted(test_lines)


def test_render_split_report_includes_key_sections():
    test_a = _StubAdapter("test_a", [_make("test_a", 1)])
    pool = _StubAdapter("pool", [_make("pool", i) for i in range(10)])
    config = SplitConfig(
        test_only_adapters=(test_a,),
        calibration_pool_adapters=(pool,),
        validation_fraction=0.20,
    )
    result = build_splits(config)
    report = render_split_report(config, result, today="2026-04-29")
    assert "# Split Report" in report
    assert "Random seed: 42" in report
    assert "## Totals" in report
    assert "## Test set per benchmark" in report
    assert "## Contamination filtering" in report
    assert "## Stratification" in report
    assert "## Pinned dataset revisions" in report
    assert "stub://test_a@frozen" in report


def test_empty_calibration_pool_yields_empty_calibration_and_validation():
    test_a = _StubAdapter("test_a", [_make("test_a", 1)])
    config = SplitConfig(
        test_only_adapters=(test_a,),
        calibration_pool_adapters=(),
        validation_fraction=0.10,
    )
    result = build_splits(config)
    assert result.stats.calibration_total == 0
    assert result.stats.validation_total == 0
    assert result.stats.test_total == 1


def test_pool_with_single_instance_falls_to_calibration_due_to_round_zero():
    """validation_fraction=0.10 over 1 instance: round(0.1) == 0 → all calib."""
    pool = _StubAdapter("pool", [_make("pool", 1)])
    config = SplitConfig(
        test_only_adapters=(),
        calibration_pool_adapters=(pool,),
        validation_fraction=0.10,
    )
    result = build_splits(config)
    assert result.stats.calibration_total == 1
    assert result.stats.validation_total == 0
