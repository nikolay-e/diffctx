# Split Report

Generated: 2026-04-29
Random seed: 42
Validation fraction: 0.1
Platform: arm64 (Darwin 25.3.0)
Note: arm64 is canonical.

## Totals

| Split | Count |
|---|---|
| Test | 1500 |
| Validation | 235 |
| Calibration | 2119 |

## Test set per benchmark

| Benchmark | Count |
|---|---|
| contextbench_verified | 500 |
| polybench500 | 500 |
| swebench_verified | 500 |

## Contamination filtering

- Pool before dedup: 3803
- Dropped (shared `(repo, base_commit)` with test): 1449
- Remaining (calibration + validation): 2354

## Stratification — `(source_benchmark, language)`

| Source | Language | Calibration | Validation |
|---|---|---|---|
| contextbench | c | 41 | 4 |
| contextbench | cpp | 45 | 5 |
| contextbench | go | 58 | 6 |
| contextbench | java | 37 | 4 |
| contextbench | javascript | 82 | 9 |
| contextbench | python | 68 | 8 |
| contextbench | rust | 39 | 4 |
| contextbench | typescript | 41 | 5 |
| multi_swebench | c | 133 | 15 |
| multi_swebench | cpp | 55 | 6 |
| multi_swebench | unknown | 12 | 1 |
| polybench | java | 34 | 4 |
| polybench | javascript | 733 | 82 |
| polybench | python | 49 | 5 |
| polybench | typescript | 512 | 57 |
| swebench_lite | python | 180 | 20 |

## Pinned dataset revisions

| Adapter | Revision |
|---|---|
| contextbench | `Contextbench/ContextBench[default]@c2855792b006af41c67202d33883fb9d46362853` |
| contextbench_verified | `Contextbench/ContextBench[contextbench_verified]@c2855792b006af41c67202d33883fb9d46362853` |
| multi_swebench | `bytedance-research/Multi-SWE-bench@85a3cf39ae22a2439f472d2525a2e07ca18809af` |
| polybench | `AmazonScience/SWE-PolyBench@d56445f9940eae4e9d2974ec66820c2f1d7754e6` |
| polybench500 | `AmazonScience/SWE-PolyBench_500@546075b4b05d17ba914e72b8f4c6d5b1ea150c1d` |
| swebench_lite | `princeton-nlp/SWE-bench_Lite@6ec7bb89b9342f664a54a6e0a6ea6501d3437cc2` |
| swebench_verified | `princeton-nlp/SWE-bench_Verified@c104f840cc67f8b6eec6f759ebc8b2693d585d4a` |
