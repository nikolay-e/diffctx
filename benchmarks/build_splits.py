"""Materialize the v1 calibration / validation / test manifests.

Default decisions (see `benchmarks/README.md` for rationale):
- 3-way split (calibration + validation + test).
- Test set: SWE-bench Verified, PolyBench Verified, Multi-SWE-bench mini,
  ContextBench verified — each kept whole, never seen during the τ sweep.
- Calibration pool: SWE-bench Lite + PolyBench500 + Multi-SWE-bench flash
  + ContextBench default. Anything sharing `(repo, base_commit)` with a
  test instance is dropped via `ContaminationDetector`.
- Stratified sampling within `(source_benchmark, language)` groups; 10% of
  each group goes to validation.
- Random seed 42 for reproducibility.

Usage::

    python -m benchmarks.build_splits --out benchmarks/manifests/v1
"""

from __future__ import annotations

import argparse
import datetime as dt
from pathlib import Path

from benchmarks.adapters import (
    BenchmarkAdapter,
    ContextBenchAdapter,
    MultiSWEBenchAdapter,
    PolyBench500Adapter,
    PolyBenchAdapter,
    SWEBenchLiteAdapter,
    SWEBenchVerifiedAdapter,
)
from benchmarks.adapters.splits import (
    SplitConfig,
    build_splits,
    render_split_report,
    write_manifests,
)


def default_test_adapters() -> tuple[BenchmarkAdapter, ...]:
    """Test set composition (frozen, never seen during the τ sweep).

    The user's original v1 plan listed "PolyBench Verified" and
    "Multi-SWE-bench mini" as test sets. Verified against the live HF API
    on 2026-04-29: neither subset is published. Substitutes:
    - `PolyBench500Adapter` (curated 500) replaces "PolyBench Verified".
    - Multi-SWE-bench has no curated subset; we put the full set in
      calibration and rely on PolyBench/SWE-bench Verified for non-Python
      and Python coverage in test.
    """
    return (
        SWEBenchVerifiedAdapter(),
        PolyBench500Adapter(),
        ContextBenchAdapter(config="contextbench_verified"),
    )


def default_calibration_pool_adapters() -> tuple[BenchmarkAdapter, ...]:
    return (
        SWEBenchLiteAdapter(),
        PolyBenchAdapter(),
        MultiSWEBenchAdapter(),
        ContextBenchAdapter(config="default"),
    )


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument(
        "--out",
        type=Path,
        default=Path(__file__).parent / "manifests" / "v1",
        help="Manifest directory (default: benchmarks/manifests/v1)",
    )
    p.add_argument("--seed", type=int, default=42)
    p.add_argument("--validation-fraction", type=float, default=0.10)
    p.add_argument(
        "--dry-run",
        action="store_true",
        help="Compute split + print SPLIT_REPORT.md to stdout, do NOT write manifests.",
    )
    args = p.parse_args()

    config = SplitConfig(
        test_only_adapters=default_test_adapters(),
        calibration_pool_adapters=default_calibration_pool_adapters(),
        validation_fraction=args.validation_fraction,
        seed=args.seed,
    )
    print(f"Building splits with seed={config.seed}, validation={config.validation_fraction:.0%}...")
    print(f"Test adapters: {[a.name for a in config.test_only_adapters]}")
    print(f"Calibration pool adapters: {[a.name for a in config.calibration_pool_adapters]}")
    print()

    result = build_splits(config)
    today = dt.date.today().isoformat()
    report = render_split_report(config, result, today=today)

    print(f"Test     : {result.stats.test_total:>5d}")
    print(f"Validation: {result.stats.validation_total:>5d}")
    print(f"Calibration: {result.stats.calibration_total:>5d}")
    print(f"Dropped (contamination): {result.stats.pool_dropped_by_contamination:>5d}")
    print()

    if args.dry_run:
        print("=== SPLIT_REPORT.md (dry-run, NOT written) ===\n")
        print(report)
        print("\n--dry-run: no manifests written. Re-run without the flag to freeze.")
        return

    written = write_manifests(result, args.out)
    report_path = args.out / "SPLIT_REPORT.md"
    report_path.write_text(report)
    print(f"Manifests: {args.out}")
    for name, path in sorted(written.items()):
        print(f"  {name}: {path}")
    print(f"Report: {report_path}")


if __name__ == "__main__":
    main()
