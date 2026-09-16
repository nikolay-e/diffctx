# Sweep results — mean file recall (and ok-instance count)

## swebench_verified

| method \ budget | 64000 |
| --- | --- |
| **ppr** | 1.000 (ok=5, n=5, ITT)|
| **ego** | 1.000 (ok=5, n=5, ITT)|
| **bm25** | 1.000 (ok=5, n=5, ITT)|
| **internal-bm25** | 1.000 (ok=5, n=5, ITT)|
| **rrf** | 1.000 (ok=5, n=5, ITT)|
| **pit** | 1.000 (ok=5, n=5, ITT)|
| **aider** | 1.000 (ok=5, n=5, ITT)|


## Headline by F-beta (mean across datasets)

| method | budget | depth | recall | precision | F0.5 | F1 | F2 | tokens p50 | tokens p95 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| **ppr** | 64000 | -1 | 1.0000 | 0.0806 | 0.0975 | 0.1429 | 0.2730 | 25380 | 25848 |
| **ego** | 64000 | -1 | 1.0000 | 0.0458 | 0.0561 | 0.0847 | 0.1752 | 20592 | 54060 |
| **bm25** | 64000 | -1 | 1.0000 | 0.0904 | 0.1104 | 0.1653 | 0.3293 | 63989 | 63998 |
| **internal-bm25** | 64000 | -1 | 1.0000 | 0.0275 | 0.0340 | 0.0531 | 0.1205 | 5520 | 33402 |
| **rrf** | 64000 | -1 | 1.0000 | 0.0301 | 0.0372 | 0.0575 | 0.1276 | 42593 | 51913 |
| **pit** | 64000 | -1 | 1.0000 | 0.0313 | 0.0386 | 0.0594 | 0.1293 | 49979 | 53278 |
| **aider** | 64000 | -1 | 1.0000 | 0.0043 | 0.0053 | 0.0085 | 0.0210 | 0 | 0 |

## Robustness — recall distribution (mean across datasets)

| method | budget | depth | %perfect | %zero | %partial | recall std |
|---|---:|---:|---:|---:|---:|---:|
| **ppr** | 64000 | -1 | 100.0 | 0.0 | 0.0 | 0.000 |
| **ego** | 64000 | -1 | 100.0 | 0.0 | 0.0 | 0.000 |
| **bm25** | 64000 | -1 | 100.0 | 0.0 | 0.0 | 0.000 |
| **internal-bm25** | 64000 | -1 | 100.0 | 0.0 | 0.0 | 0.000 |
| **rrf** | 64000 | -1 | 100.0 | 0.0 | 0.0 | 0.000 |
| **pit** | 64000 | -1 | 100.0 | 0.0 | 0.0 | 0.000 |
| **aider** | 64000 | -1 | 100.0 | 0.0 | 0.0 | 0.000 |

## Latency — elapsed_seconds across datasets

| method | budget | depth | mean | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|---:|
| **ppr** | 64000 | -1 | 4.32 | 4.42 | 4.77 | 4.79 |
| **ego** | 64000 | -1 | 4.89 | 4.87 | 5.60 | 5.65 |
| **bm25** | 64000 | -1 | 5.54 | 5.64 | 5.72 | 5.72 |
| **internal-bm25** | 64000 | -1 | 2.93 | 3.02 | 3.24 | 3.25 |
| **rrf** | 64000 | -1 | 5.69 | 5.87 | 6.33 | 6.38 |
| **pit** | 64000 | -1 | 5.48 | 5.59 | 6.07 | 6.07 |
| **aider** | 64000 | -1 | 26.89 | 25.81 | 30.18 | 30.85 |

## Selection cardinality (files / fragments)

| method | budget | depth | n_selected p50 | n_selected p95 | n_gold p50 |
|---|---:|---:|---:|---:|---:|
| **ppr** | 64000 | -1 | 25.0 | 33.8 | 1.0 |
| **ego** | 64000 | -1 | 28.0 | 82.8 | 1.0 |
| **bm25** | 64000 | -1 | 11.0 | 15.8 | 1.0 |
| **internal-bm25** | 64000 | -1 | 38.0 | 189.4 | 1.0 |
| **rrf** | 64000 | -1 | 40.0 | 103.8 | 1.0 |
| **pit** | 64000 | -1 | 35.0 | 148.0 | 1.0 |
| **aider** | 64000 | -1 | 223.0 | 270.0 | 1.0 |

## Pipeline latency breakdown (median, ms)

| method | budget | depth | parse | discover | tokenize | scoring | selection |
|---|---:|---:|---:|---:|---:|---:|---:|
| **ppr** | 64000 | -1 | 4.9 | 349.6 | 768.4 | 148.6 | 15.6 |
| **ego** | 64000 | -1 | 5.7 | 483.4 | 732.3 | 155.9 | 32.4 |
| **bm25** | 64000 | -1 | — | — | — | — | — |
| **internal-bm25** | 64000 | -1 | 4.0 | 320.7 | 645.9 | 431.8 | 250.2 |
| **rrf** | 64000 | -1 | 5.8 | 467.3 | 681.7 | 852.8 | 271.9 |
| **pit** | 64000 | -1 | 4.9 | 450.0 | 811.9 | 790.2 | 272.3 |
| **aider** | 64000 | -1 | — | — | — | — | — |

## Graph size — edges and pushes (median per instance)

| method | budget | depth | candidates | edges | edges_dropped | nodes_capped | ppr_fwd | ppr_bwd |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| **ppr** | 64000 | -1 | 567 | 512671 | 142757 | 1589 | 1065 | 618 |
| **ego** | 64000 | -1 | 1514 | 512671 | 142757 | 1589 | 0 | 0 |
| **bm25** | 64000 | -1 | — | — | — | — | — | — |
| **internal-bm25** | 64000 | -1 | 14531 | 0 | 0 | 0 | 0 | 0 |
| **rrf** | 64000 | -1 | 14541 | 512671 | 142757 | 1589 | 0 | 0 |
| **pit** | 64000 | -1 | 14541 | 512671 | 142757 | 1589 | 0 | 0 |
| **aider** | 64000 | -1 | — | — | — | — | — | — |

## Recall stratified by |gold| (file count)

Buckets reflect how many files the gold patch touches; method must scale across all of them to be useful.

| method | budget | depth | 1 | 2-3 | 4-7 | 8-15 | 16+ |
|---|---:|---:|---:|---:|---:|---:|---:|
| **ppr** | 64000 | -1 | 1.000 | — | — | — | — |
| **ego** | 64000 | -1 | 1.000 | — | — | — | — |
| **bm25** | 64000 | -1 | 1.000 | — | — | — | — |
| **internal-bm25** | 64000 | -1 | 1.000 | — | — | — | — |
| **rrf** | 64000 | -1 | 1.000 | — | — | — | — |
| **pit** | 64000 | -1 | 1.000 | — | — | — | — |
| **aider** | 64000 | -1 | 1.000 | — | — | — | — |

## Recall stratified by difficulty ratio |gold|/|changed|

Ratio≈1 means gold is the diff itself (trivial). Ratio>1 means real retrieval is needed.

| method | budget | depth | ≤1.0 | 1.0-1.5 | 1.5-2.0 | 2.0-3.0 | 3.0+ |
|---|---:|---:|---:|---:|---:|---:|---:|
| **ppr** | 64000 | -1 | 1.000 | — | — | — | — |
| **ego** | 64000 | -1 | 1.000 | — | — | — | — |
| **bm25** | 64000 | -1 | 1.000 | — | — | — | — |
| **internal-bm25** | 64000 | -1 | 1.000 | — | — | — | — |
| **rrf** | 64000 | -1 | 1.000 | — | — | — | — |
| **pit** | 64000 | -1 | 1.000 | — | — | — | — |
| **aider** | 64000 | -1 | 1.000 | — | — | — | — |

## Gold characterization (per dataset, from any cell)

| dataset | %single-file | %multi-file | %whole-file | %zero-gold |
|---|---:|---:|---:|---:|
| swebench_verified | 100.0 | 0.0 | 0.0 | 0.0 |

## Per-language headline (top languages by instance count)

Each cell shows `recall / F1 / F2` for that (method, budget, depth) on that language.

| config | python |
|---|---|
| **ppr** b=64000 L=-1 | 1.000 / 0.143 / 0.273 |
| **ego** b=64000 L=-1 | 1.000 / 0.085 / 0.175 |
| **bm25** b=64000 L=-1 | 1.000 / 0.165 / 0.329 |
| **internal-bm25** b=64000 L=-1 | 1.000 / 0.053 / 0.121 |
| **rrf** b=64000 L=-1 | 1.000 / 0.058 / 0.128 |
| **pit** b=64000 L=-1 | 1.000 / 0.059 / 0.129 |
| **aider** b=64000 L=-1 | 1.000 / 0.009 / 0.021 |
