from __future__ import annotations

import platform
import random
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path

from benchmarks.adapters.base import BenchmarkAdapter, BenchmarkInstance
from benchmarks.adapters.contamination import ContaminationDetector

_TWO_COL_DIVIDER = "|---|---|"


@dataclass(frozen=True)
class SplitConfig:
    """Inputs for the train/validation/test partition.

    The 3-way split is deliberate: calibration drives the τ sweep, validation
    is the held-out signal that picks the best τ from a small candidate set,
    test is paper-grade and untouched until the very end.
    """

    test_only_adapters: tuple[BenchmarkAdapter, ...]
    """Their entire output goes to the test set; never to calibration."""

    calibration_pool_adapters: tuple[BenchmarkAdapter, ...]
    """Pool from which calibration + validation are stratified-sampled."""

    validation_fraction: float = 0.10
    """Fraction of the (post-contamination) pool that becomes validation."""

    seed: int = 42

    @property
    def test_adapter_names(self) -> tuple[str, ...]:
        return tuple(a.name for a in self.test_only_adapters)


@dataclass(frozen=True)
class SplitResult:
    test_ids: dict[str, frozenset[str]]
    """Per-test-benchmark instance_ids. Map preserves which benchmark each
    test instance came from so the paper table can break out per-source."""

    validation_ids: frozenset[str]
    calibration_ids: frozenset[str]
    stats: SplitStats


@dataclass(frozen=True)
class SplitStats:
    test_total: int
    test_per_benchmark: dict[str, int]
    calibration_total: int
    validation_total: int
    pool_before_dedup: int
    pool_dropped_by_contamination: int
    per_stratum: dict[tuple[str, str], tuple[int, int]] = field(default_factory=dict)
    """`(source_benchmark, language) -> (calibration_count, validation_count)`."""

    dataset_revisions: dict[str, str] = field(default_factory=dict)


def _stratify_key(instance: BenchmarkInstance) -> tuple[str, str]:
    return (instance.source_benchmark, instance.language or "unknown")


def build_splits(config: SplitConfig) -> SplitResult:
    """Carve a 3-way partition: test (frozen, never seen during sweep),
    validation (held-out for model selection), calibration (sweep target).

    Steps:
    1. Test set = union of every instance from `test_only_adapters`.
    2. Build `ContaminationDetector` over ALL adapters so cross-benchmark
       (repo, base_commit) twins are visible.
    3. Calibration pool = `calibration_pool_adapters` minus any instance
       contaminated with a test instance.
    4. Stratified shuffle within `(source_benchmark, language)` groups,
       slicing `validation_fraction` off the front for validation.
    """
    test_ids: dict[str, set[str]] = {}
    test_global: set[str] = set()
    revisions: dict[str, str] = {}
    for adapter in config.test_only_adapters:
        revisions[adapter.name] = adapter.dataset_revision()
        ids = {inst.instance_id for inst in adapter.load()}
        test_ids[adapter.name] = ids
        test_global |= ids

    detector = ContaminationDetector()
    for adapter in (*config.test_only_adapters, *config.calibration_pool_adapters):
        detector.ingest(adapter)

    raw_pool: list[BenchmarkInstance] = []
    for adapter in config.calibration_pool_adapters:
        revisions.setdefault(adapter.name, adapter.dataset_revision())
        raw_pool.extend(adapter.load())
    pool_before = len(raw_pool)
    safe_pool = detector.filter_calibration_pool(raw_pool, test_global)
    dropped = pool_before - len(safe_pool)

    grouped: dict[tuple[str, str], list[BenchmarkInstance]] = defaultdict(list)
    for inst in safe_pool:
        grouped[_stratify_key(inst)].append(inst)

    rng = random.Random(config.seed)
    calibration: set[str] = set()
    validation: set[str] = set()
    per_stratum: dict[tuple[str, str], tuple[int, int]] = {}
    for key in sorted(grouped):
        bucket = sorted(grouped[key], key=lambda i: i.instance_id)
        rng.shuffle(bucket)
        n_val = round(len(bucket) * config.validation_fraction)
        val_part = bucket[:n_val]
        cal_part = bucket[n_val:]
        validation.update(i.instance_id for i in val_part)
        calibration.update(i.instance_id for i in cal_part)
        per_stratum[key] = (len(cal_part), len(val_part))

    return SplitResult(
        test_ids={name: frozenset(ids) for name, ids in test_ids.items()},
        validation_ids=frozenset(validation),
        calibration_ids=frozenset(calibration),
        stats=SplitStats(
            test_total=len(test_global),
            test_per_benchmark={name: len(ids) for name, ids in test_ids.items()},
            calibration_total=len(calibration),
            validation_total=len(validation),
            pool_before_dedup=pool_before,
            pool_dropped_by_contamination=dropped,
            per_stratum=per_stratum,
            dataset_revisions=revisions,
        ),
    )


def render_split_report(config: SplitConfig, result: SplitResult, today: str = "") -> str:
    """Markdown summary suitable for committing alongside the manifests."""
    lines: list[str] = []
    lines.append("# Split Report")
    lines.append("")
    if today:
        lines.append(f"Generated: {today}")
    lines.append(f"Random seed: {config.seed}")
    lines.append(f"Validation fraction: {config.validation_fraction}")
    lines.append(f"Platform: {platform.machine()} ({platform.system()} {platform.release()})")
    lines.append(
        "Note: arm64 is the canonical platform for diffctx benchmarks on this "
        "project — Rosetta'd amd64 on Apple Silicon is ~25% slower for a "
        "sub-precision float-determinism win the paper does not require."
    )
    lines.append("")
    lines.append("## Totals")
    lines.append("")
    lines.append("| Split | Count |")
    lines.append(_TWO_COL_DIVIDER)
    lines.append(f"| Test | {result.stats.test_total} |")
    lines.append(f"| Validation | {result.stats.validation_total} |")
    lines.append(f"| Calibration | {result.stats.calibration_total} |")
    lines.append("")
    lines.append("## Test set per benchmark")
    lines.append("")
    lines.append("| Benchmark | Count |")
    lines.append(_TWO_COL_DIVIDER)
    for name in sorted(result.stats.test_per_benchmark):
        lines.append(f"| {name} | {result.stats.test_per_benchmark[name]} |")
    lines.append("")
    lines.append("## Contamination filtering")
    lines.append("")
    lines.append(f"- Pool before dedup: {result.stats.pool_before_dedup}")
    lines.append(f"- Dropped (shared `(repo, base_commit)` with test): {result.stats.pool_dropped_by_contamination}")
    lines.append(f"- Remaining (calibration + validation): {result.stats.calibration_total + result.stats.validation_total}")
    lines.append("")
    lines.append("## Stratification — `(source_benchmark, language)`")
    lines.append("")
    lines.append("| Source | Language | Calibration | Validation |")
    lines.append("|---|---|---|---|")
    for key in sorted(result.stats.per_stratum):
        cal, val = result.stats.per_stratum[key]
        lines.append(f"| {key[0]} | {key[1]} | {cal} | {val} |")
    lines.append("")
    lines.append("## Pinned dataset revisions")
    lines.append("")
    lines.append("| Adapter | Revision |")
    lines.append(_TWO_COL_DIVIDER)
    for name in sorted(result.stats.dataset_revisions):
        lines.append(f"| {name} | `{result.stats.dataset_revisions[name]}` |")
    lines.append("")
    return "\n".join(lines)


def write_manifests(result: SplitResult, root: Path) -> dict[str, Path]:
    """Persist every split as a sorted line-per-id text file under `root/`.

    Returns the map `manifest_name -> path` so the caller can reference paths
    in downstream scripts.
    """
    out: dict[str, Path] = {}
    root.mkdir(parents=True, exist_ok=True)
    for name, ids in result.test_ids.items():
        path = root / f"test_{name}.txt"
        path.write_text("\n".join(sorted(ids)) + "\n")
        out[f"test_{name}"] = path
    (root / "validation.txt").write_text("\n".join(sorted(result.validation_ids)) + "\n")
    out["validation"] = root / "validation.txt"
    (root / "calibration.txt").write_text("\n".join(sorted(result.calibration_ids)) + "\n")
    out["calibration"] = root / "calibration.txt"
    return out
