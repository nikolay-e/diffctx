# Benchmarks

Catalog of every evaluation, diagnostic, and orchestration script under
`benchmarks/` and `scripts/`. This file is a reference — for the *why*
of each benchmark, see the project paper and `CLAUDE.md`.

## TL;DR — when to use what

| You want to … | Use |
|---|---|
| Single ContextBench run | `python -m benchmarks cb --limit 50 --budget 8000` |
| Diagnose why diffctx missed a file on a specific instance | `python -m benchmarks cb --forensic --limit 5` |
| Test robustness via leave-one-out | `python -m benchmarks loo --limit 50` |
| Sweep budget across modes | `python -m benchmarks curve --limit 50` |
| Combine N seed runs into mean ± std | `python -m benchmarks aggregate results/*.json` |
| Compare two result sets (A/B) | `python benchmarks/compare_runs.py after.json before.json` |
| Probe one-at-a-time parameter sensitivity | `python scripts/sensitivity_check.py` |
| Run the full 12-config sweep (4 modes × 3 budgets) | `bash scripts/sweep_orchestrator.sh` |
| Generate the markdown summary report | `python scripts/aggregate_sweep_report.py` |

## Datasets

### ContextBench (primary)

Loaded via HuggingFace Hub: `Contextbench/ContextBench`.

| Config | Flag | Approx. size | Use for |
|---|---|---|---|
| `default` | `--dataset full` | ~672 nontrivial | Sweeps, calibration |
| `contextbench_verified` | `--dataset verified` | curated subset | Final paper numbers |

Each instance has: `instance_id`, `repo`, `repo_url`, `base_commit`,
`patch`, `gold_context` (list of `{path, lines}`), `language`.

Repo cache is persisted at `~/.cache/contextbench_repos` (override via
`CONTEXTBENCH_REPOS_DIR`).

### SWE-bench-derived LOO

`benchmarks/loo_swebench.py` reuses ContextBench instances but treats
them as a robustness probe: hide one patch file, ask diffctx to recover
it from the remaining context.

## Scripts

### `benchmarks/__main__.py` — CLI dispatcher

```bash
python -m benchmarks <subcommand> [args]
```

| Subcommand | Routes to |
|---|---|
| `cb` | `contextbench_diffctx.py` (or `forensic_contextbench.py` if `--forensic`) |
| `loo` | `loo_swebench.py` |
| `compare` | `compare_runs.py` |
| `curve` | `budget_curve.py` |
| `aggregate` | `aggregate_seeds.py` |

### `contextbench_diffctx.py` — main evaluation

**Purpose**: end-to-end recall/precision on ContextBench, parallel
across instances, multi-seed.

**Args**:

| Flag | Default | Meaning |
|---|---|---|
| `--limit` | 3 | Number of instances |
| `--budget` | 16000 | Token budget passed to diffctx |
| `--lang` | none | Filter by `language` field |
| `--nontrivial-only` | true | Skip instances where gold ⊆ patch files |
| `--seeds` | `42` | Comma-separated seeds for shuffle |
| `--no-shuffle` | false | Use dataset order |
| `--scoring` | `hybrid` | `hybrid` / `ppr` / `ego` / `bm25` |
| `--baseline` | `treemapper` | `treemapper` / `patch_files` / `bm25` |
| `--dataset` | `full` | `full` / `verified` |
| `--tau` | 0.08 | Selection stopping threshold |

**Metrics**: `file_recall`, `file_precision`, `nontrivial_file_recall`,
`line_recall`, `line_recall_nontrivial`, `elapsed_s`, `fragment_count`.
Bootstrap 95% CI on each. Per-language and per-repo breakdowns.

**Output**: `results/cb_{scoring}_n{limit}_b{budget}[_s{seed}].json`
plus stdout tables. One file per seed when `--seeds` lists multiple.

### `forensic_contextbench.py` — diagnostic mode

**Purpose**: trace through pipeline stages on individual instances,
classify why each nontrivial gold file was missed (universe →
fragmented → candidate → selected).

**Invocation**: `python -m benchmarks cb --forensic --limit 5`

**Output**:

- Stdout: per-instance trace + stage-wise breakdown table + alerts on
  `patch_coverage < 0.95`.
- `/tmp/diffctx_dump/`: `universe.txt`, `fragmented.txt`, `selected.txt`,
  `candidates.txt`, `diffctx_scores.jsonl` (set via env vars
  `DIFFCTX_DUMP_DIR`, `DIFFCTX_DUMP_SCORES`).

**When to use**: a recall regression appears in the main eval; pick a
failing instance ID and run forensic to see at which pipeline stage
the gold file disappeared.

### `loo_swebench.py` — leave-one-out robustness

**Purpose**: hide one file from the ground truth, run diffctx, check
whether it appears in the selected set. Plus a distractor check (random
file with same suffix) for false-positive rate.

**Args**:

| Flag | Default | Meaning |
|---|---|---|
| `--limit` | 50 | Instances |
| `--budget` | 16000 | Token budget |
| `--seed` / `--seeds` | 42 | RNG |
| `--dataset` | `Contextbench/ContextBench` | HF path |
| `--split` | `contextbench_verified` | HF config |
| `--scoring` | `ego` | Mode |
| `--timeout` | 300s | Per-instance timeout |

**Filtering**: only multi-file patches; mechanical / vendor /
generated paths excluded.

**Metrics per trial**: `found` (hidden file recovered), `found_distractor`
(false positive), `n_patch_files`, `n_remaining`, `n_selected`.

**Output**: `results/loo_{scoring}_n{limit}_b{budget}[_s{seed}].json` +
stdout: per-repo / per-language % found.

### `budget_curve.py` — budget × mode sweep

**Purpose**: how does recall scale with token budget?

**Args**: `--limit` (default 50).

**Sweep**:

- Budgets: `[8000, 16000, 32000, 64000, 999999]`
- Modes: `[hybrid, ppr, ego]`

**Workflow**: spawns one `python -m benchmarks cb` subprocess per
(budget, mode) pair, skips configs whose result file already exists,
then aggregates into one curve.

**Output**: `results/curve.json` with per-mode list of
`{budget, n, nontrivial_file_recall, file_recall, line_recall}`.

### `aggregate_seeds.py` — multi-seed mean ± std

**Purpose**: combine per-seed JSON files into cross-seed statistics.

**Invocation**: `python -m benchmarks aggregate file1.json file2.json …`

**Metrics aggregated**: `file_recall`, `file_precision`,
`nontrivial_file_recall`, `line_recall`, `line_recall_nontrivial`.

**Output**: stdout — per-seed line + cross-seed `mean ± stdev` row.

### `compare_runs.py` — paired A/B test

**Purpose**: statistical test between two result sets (e.g. before/after
a tuning change).

**Invocation**: `python benchmarks/compare_runs.py <after.json> <before.json>`

**Statistics**:

- Bootstrap 95% CI per metric per group (n_iter=10000).
- Paired bootstrap delta with p-value.
- Wilcoxon signed-rank test (`scipy.stats.wilcoxon`).

**Output**: stdout table —
`metric | before CI | after CI | delta CI | p_boot | p_wilc`.

### `stats.py` — statistics helpers

Library, not a script. Used by `compare_runs.py` and others.

| Function | Returns |
|---|---|
| `bootstrap_ci(values, n_iter=10000, alpha=0.05, seed=42)` | `(mean, lo, hi)` |
| `paired_bootstrap_delta(before, after, n_iter=10000, seed=42)` | `{delta_mean, ci_lo, ci_hi, p_value}` |
| `wilcoxon_paired(before, after)` | `{statistic, p_value}` |

### `common.py` — shared utilities

Library used by every script.

Highlights:

- `repos_dir(...)`: cache directory resolver.
- `ensure_repo(url, name, commit, target_dir)`: git worktree + checkout.
- `apply_as_commit(...) / reset_to_parent(...)`: patch ↔ commit cycle.
- `run_parallel(fn, args, WORKERS)`: thread-pool executor.
- `save_results(results, tag, seed, budget, scoring, baseline)`:
  uniform JSON writer to `results/`.
- `warm_cache(instances)`: pre-clone + fetch all repos before parallel.
- `WORKERS`: env var `BENCH_WORKERS` (default 11).

## Orchestration scripts (`scripts/`)

### `scripts/sweep_orchestrator.sh` — full sweep

Runs the 12-config matrix (4 modes × 3 budgets) inside Docker with
adaptive resource scaling. Skips configs whose result file already has
≥550 ok runs. Tries two resource tiers if the first fails:
`(workers=7, batch=20)` → `(workers=4, batch=12)`.

**Env**: `BENCH_WORKERS`, `BENCH_BATCH_SIZE`.

**Output**:

- `results/cb_{mode}_n9999_b{budget}.json` — one per config.
- `results/logs/sweep_orchestrator.log` — orchestration log.

After all configs complete, calls `aggregate_sweep_report.py`.

### `scripts/aggregate_sweep_report.py` — markdown summary

Reads the 12 sweep result files; produces `results/SWEEP_REPORT.md`
with sections:

- Per-config summary (ok count, failure breakdown, metrics).
- Head-to-head by budget (mode × budget recall matrix).
- Budget impact per mode.
- Per-language `nontrivial_file_recall` at `b=16000`.
- Raw files index.

### `scripts/sensitivity_check.py` — parameter sensitivity

One-at-a-time perturbation of the 15 Group-C operational parameters.
Pertubation factors `[0.50, 0.75, 1.25, 1.50]` → 61 runs total
(1 baseline + 15 × 4).

**Args**:

| Flag | Default |
|---|---|
| `--diff` | `HEAD~5..HEAD` |
| `--budget` | 4096 |
| `--repo` | `.` |
| `--params` | all 15 |

**Output**: stdout table — `param | factor | value | tokens | Δ% | Jaccard`.

**Limitation**: runs on a single diff (the local repo's HEAD~5..HEAD
by default). Use as smoke test, not as ground truth for parameter
optimization. For real calibration use ContextBench.

## Output directory layout

```text
results/
├── cb_{scoring}_n{limit}_b{budget}.json      # contextbench_diffctx
├── cb_{scoring}_n{limit}_b{budget}_s{seed}.json   # multi-seed runs
├── loo_{scoring}_n{limit}_b{budget}.json     # leave-one-out
├── curve.json                                 # budget_curve aggregate
├── SWEEP_REPORT.md                            # aggregated sweep report
└── logs/
    └── sweep_orchestrator.log
```

JSON record schema (per instance, abbreviated):

```json
{
  "id": "<repo>__<sha>",
  "status": "ok | clone_fail | timeout | error",
  "language": "python",
  "repo": "owner/name",
  "elapsed_s": 12.3,
  "fragment_count": 87,
  "file_recall": 0.91,
  "file_precision": 0.42,
  "nontrivial_file_recall": 0.83,
  "line_recall": 0.76,
  "line_recall_nontrivial": 0.71
}
```

## Reproducibility

- **Seeds**: every script that shuffles takes `--seed` / `--seeds`. Default
  42. Multi-seed runs append `_s{seed}` to the JSON filename.
- **tiktoken**: pinned to `==0.12.0` in `pyproject.toml` and to `=0.6.0`
  for `tiktoken-rs` in `diffctx/Cargo.toml`. Drift snapshot in
  `tests/test_diffctx_invariants.py::test_tiktoken_o200k_base_encoding_is_pinned`.
- **Determinism**: `tests/test_diffctx_invariants.py` locks byte-identical
  output across runs and rayon thread counts.
- **Worker counts**: `BENCH_WORKERS` (Python pool, default 11),
  `RAYON_NUM_THREADS` (Rust pool — common.py sets to 1 by default to
  avoid oversubscription with the Python pool).

## Pre-flight & resilience

Every CLI runner (`run_eval`, `calibrate`, `select_final`, `run_final_eval`)
performs a pre-flight check before submitting any work:

- **Memory probe**: reads `/proc/meminfo` (Linux only — silent on macOS).
  Exits with code 2 if visible memory is below `--min-memory-gb` (default
  16 GB). Closes the macOS Docker Desktop "VM defaults to 8 GB" footgun.
- **Disk probe**: warns (does not exit) when the repos volume has less
  than `--min-disk-gb` (default 50 GB). SWE-bench `transformers` alone is
  ~4 GB; calibration touches 10-20 distinct repos.

`runner.run_eval_set` exposes three resilience knobs threaded through every
sweep CLI:

| Flag | Behaviour |
|---|---|
| `--timeout-per-instance N` | Records `status="timeout"` for any future that does not return within N seconds. Hung worker threads are abandoned at pool shutdown (Python cannot kill threads safely). |
| `--resume-from FILE.jsonl` | Skips instance_ids already present in the JSONL checkpoint — restart after a crash continues where it left off. |
| `--checkpoint FILE.jsonl` | Appends each completed result to the JSONL as it arrives, so a crash mid-sweep loses at most one in-flight result. |

`calibrate` uses one checkpoint per grid cell at `<out>/checkpoints/<label>.jsonl`;
`run_final_eval` uses one per test set at `<out>/<benchmark>.checkpoint.jsonl`.

## Canonical platform

**arm64** is canonical for both development and the paper artifact on this
project. Float-determinism deltas across arm64 / amd64 are below paper-reported
precision (3 sig figs); Rosetta'd amd64 on Apple Silicon is ~25% slower for
no scientifically defensible win. `SPLIT_REPORT.md` records `platform.machine()`
on the build host so any reviewer can verify the platform of the pinned
manifests.

## Reproducibility stack

| Layer | What pins it |
|---|---|
| Rust toolchain | `rust-toolchain.toml` (`channel = "1.92.0"`) |
| Cargo deps | `diffctx/Cargo.lock` (committed) |
| Python deps | `requirements-bench.lock` from `uv pip compile`; install with `pip install --require-hashes -r requirements-bench.lock` (committed) |
| HuggingFace datasets | `benchmarks/dataset_revisions.json` from `python -m benchmarks.pin_revisions` (committed) |
| tiktoken BPE | `tiktoken==0.12.0` + `tiktoken-rs=0.6.0` exact-pinned; drift snapshot test `test_tiktoken_o200k_base_encoding_is_pinned` |
| Build determinism | NOT bit-for-bit — Rust release builds carry HashMap ordering non-determinism per `cargo#16693`. Documented limitation; reviewers don't ask for byte-identical `.so`. |

## Multi-benchmark adapter layer

`benchmarks/adapters/` normalizes heterogeneous benchmark sources behind a
single `BenchmarkAdapter` interface, so calibration and evaluation can mix
SWE-bench Lite, SWE-bench Verified, ContextBench, and (future) PolyBench /
Multi-SWE-bench instances without per-source branching in the runner.

| Module | Purpose |
|---|---|
| `adapters/base.py` | `GoldenFragment`, `BenchmarkInstance`, `EvalResult`, `BenchmarkAdapter` ABC |
| `adapters/swebench.py` | `SWEBenchLiteAdapter`, `SWEBenchVerifiedAdapter` (princeton-nlp) |
| `adapters/polybench.py` | `PolyBenchAdapter`, `PolyBench500Adapter`, `PolyBenchVerifiedAdapter` (amazon-science, CST node-level annotations) |
| `adapters/multi_swebench.py` | `MultiSWEBenchAdapter`, `MultiSWEBenchMiniAdapter`, `MultiSWEBenchFlashAdapter` (ByteDance, Java/TS/JS/Go/Rust/C/C++; language inferred from file extension when missing) |
| `adapters/contextbench.py` | `ContextBenchAdapter(config="default" \| "contextbench_verified")` with fragment-level annotations |
| `adapters/contamination.py` | `ContaminationDetector` — cross-benchmark dedup by `(repo, base_commit)` |
| `adapters/evaluator.py` | `UniversalEvaluator`, `SelectionOutput` — file/fragment/line metrics, per-benchmark aggregation |

**Why contamination matters**: ContextBench is built from SWE-bench Verified
∪ PolyBench ∪ Multi-SWE-bench. Calibrating on ContextBench while testing on
SWE-bench Verified is direct leakage. The detector indexes every adapter's
instances by `(repo, base_commit)`, then `filter_calibration_pool(...)` drops
any candidate that shares state with a held-out test instance.

**Adapter contract**:

```python
class BenchmarkAdapter(ABC):
    name: str
    @abstractmethod
    def dataset_revision(self) -> str: ...    # pinned for reproducibility
    @abstractmethod
    def _load_raw(self) -> Iterator[dict]: ...  # network I/O lives here
    @abstractmethod
    def _normalize(self, row) -> BenchmarkInstance | None: ...  # pure
    def load(self) -> Iterator[BenchmarkInstance]: ...  # final
```

`load()` is pure normalization; tests stub `_load_raw()` with synthetic rows
to verify field mapping without HF fetches (`tests/test_benchmark_adapters.py`).

**Universal evaluator**:

```python
from benchmarks.adapters import UniversalEvaluator, SelectionOutput

ev = UniversalEvaluator()
result = ev.evaluate(
    instance,
    SelectionOutput(
        selected_files=frozenset(selected_paths),
        selected_fragments=tuple_of_GoldenFragment,  # optional
        used_tokens=N,
        elapsed_seconds=t,
    ),
    budget=8000,
)
# result.file_recall, result.file_precision   — always
# result.fragment_recall, .fragment_precision — when gold_fragments present
# result.line_f1                              — line-set F1 averaged over files
```

`ev.aggregate_per_benchmark(results)` groups by `source_benchmark`. The
calibration objective is `min(per_benchmark_recall)` — generalization-friendly,
prevents one large benchmark from dominating the global mean.

**Pinning workflow** (mandatory before any calibration sweep):

```bash
python -m benchmarks.pin_revisions   # writes benchmarks/dataset_revisions.json
git add benchmarks/dataset_revisions.json
git commit -m "chore(benchmarks): pin dataset revisions for v1"
```

Resolution order in `adapters/dataset_pins.py::resolve_revision`:

1. `BENCH_REVISION_<HF_PATH_UPPER_SAFE>` env var (one-off override)
2. `benchmarks/dataset_revisions.json` (committed pin file)
3. `default="main"` (development only, NOT reproducible)

Each adapter takes an optional `revision="..."` kwarg that wins over both.

**Pinned revisions** (canonical HF paths registered with `dataset_pins`):

| Adapter | HF path |
|---|---|
| `SWEBenchLiteAdapter` | `princeton-nlp/SWE-bench_Lite` |
| `SWEBenchVerifiedAdapter` | `princeton-nlp/SWE-bench_Verified` |
| `PolyBench{,500,Verified}Adapter` | `AmazonScience/SWE-PolyBench` (configs: `default`, `polybench500`, `verified`) |
| `MultiSWEBench{,Mini,Flash}Adapter` | `bytedance-research/Multi-SWE-bench` (configs: `default`, `mini`, `flash`) |
| `ContextBenchAdapter` | `Contextbench/ContextBench` (configs: `default`, `contextbench_verified`) |

## Calibration pipeline

Top-down: a single CLI per phase, each consuming the previous phase's output.
All four scripts live in `benchmarks/` and read manifests from
`benchmarks/manifests/v1/` produced by `build_splits.py`.

| Phase | Script | Input | Output |
|---|---|---|---|
| Build splits | `python -m benchmarks.build_splits` | adapters | `manifests/v1/{calibration,validation,test_*}.txt` + `SPLIT_REPORT.md` |
| One-off run | `python -m benchmarks.run_eval --manifest M --tau X --core-budget-fraction Y --out R.json` | manifest | per-instance results JSON |
| 2D grid sweep | `python -m benchmarks.calibrate --manifest calibration.txt --tau 0.04,0.08,0.12,0.16 --core-budget-fraction 0.5,0.6,0.7,0.8 --out results/calibration/v1` | calibration manifest | `grid_results.json`, `top_candidates.json`, `grid_report.md` |
| Validation pass | `python -m benchmarks.select_final --candidates top_candidates.json --manifest validation.txt --out final_choice.json` | top-K candidates + validation manifest | `final_choice.json` (winner + per-benchmark scores) |
| Final eval | `python -m benchmarks.run_final_eval --winner final_choice.json --manifests-dir manifests/v1 --out results/final/v1` | winner + every test manifest | per-test-set JSONs + `PAPER_TABLE.md` |

The objective at sweep time is `min(per_benchmark file_recall)` —
generalization-friendly. Tie-breaking on `top_k_trials` prefers the trial
with lower mean tokens (cheaper context wins under equal recall).

`benchmarks/diffctx_eval_fn.py` is the only file that bridges the adapter
layer to the actual diffctx pipeline: `make_diffctx_eval_fn(repos_dir)`
returns an `EvalFn(instance, params) -> EvalResult` that clones the repo,
applies the gold patch as a commit, sets the env vars from `params.to_env()`,
calls `build_diff_context`, computes metrics via `UniversalEvaluator`, and
reverts. Tests pass a stub `EvalFn` and never touch this module.

## Calibration vs evaluation split

To prevent contamination between hyperparameter calibration (e.g.
sweeping `tau`) and final paper numbers, **never share instances
between the two phases**. Recommended split:

| Phase | Dataset | Use |
|---|---|---|
| Calibration | `default` minus `verified` | parameter tuning |
| Evaluation | `contextbench_verified` | paper figures |

Concrete instance manifests live under `benchmarks/manifests/` (when
present); both files are committed to git so the split is reproducible
across runs and reviewers. The `verified` config is the
ContextBench-team-curated holdout — using it as eval keeps the split
under external authority rather than our own choice.

For the 1D `tau` calibration specifically: 30–50 stratified instances
from the calibration pool are sufficient (BO converges fast on a 1D
continuous space). Verify on the full calibration set after best `tau`
is found, then evaluate ONCE on the held-out `verified` set.
