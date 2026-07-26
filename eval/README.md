# Evaluation framework

Catalog of evaluation, diagnostic, and orchestration code under `eval/` and
`scripts/`. `evaluation` names the whole subsystem; `benchmark` is reserved
for a particular dataset or measured execution. For the rationale behind each
benchmark, see the project paper and `CLAUDE.md`.

## TL;DR — when to use what

| You want to … | Use |
|---|---|
| Final eval / any baseline / budget grid (CURRENT pipeline) | `python -m eval run-final --winner ... --manifests-dir datasets/eval-splits/v1 --out results/...` |
| Ablation cells | `run_final_eval` flags: `--scoring ego\|ppr\|bm25` (internal-BM25 cell), `--tau 0` (stopping off), `--extra-env DIFFCTX_EGO_LEXICAL_EPS=0` / `DIFFCTX_OBJECTIVE=boltzmann` / `DIFFCTX_RELATEDNESS_BONUS=0` (repeatable; applied around the heavy phase in the multi-budget reuse path) |
| Floor baselines | `--baseline patch_files` (changed-files-only) and `--baseline random` (seeded-random packing on the BM25 protocol, `DIFFCTX_RANDOM_BASELINE_SEED`) |
| Aider baselines | `--baseline aider_fair\|aider_oracle --aider-request-timeout 600` (separate from the diffctx kill-switch `--timeout-per-instance`) |
| Bit-equivalence gate (mandatory for perf refactors) | `python -m eval equivalence --a <run_old> --b <run_new>` — identical selected sets / used_tokens / metrics; baseline sample in `results/sweep_v2_local/equiv/` |
| Full sweep (CI) | `.github/workflows/eval-sweep.yml` (`workflow_dispatch`, mode=smoke\|full) |
| Per-cell metrics / sweep aggregation | `python -m eval cell-metrics ...` / `python -m eval aggregate-sweep ...` |
| Probe one-at-a-time parameter sensitivity | `bash scripts/sensitivity_check.sh` |

Protocol notes: per-instance timeout for evaluation runs is **600s**;
sweeps use **900s** to keep the near-dense #116 class measured rather
than censored
(`--timeout-per-instance 600`; calibrate.py defaults to 20s — always pass the
flag). Multi-SWE-bench streaming fails deterministically on a malformed shard
at the pinned revision; calibration/validation runs need
`MULTISWE_ALLOW_TRUNCATED=1` (the v1 manifests were built from the same loadable
prefix). Progress `eta~` is a rolling-window estimate; the final `tail of N`
line is bounded by the per-instance timeout, not extrapolated. Failure rows
carry `repo`/`language` for cluster diagnostics; ok rows additionally carry
`selected_files`, `nontrivial_file_recall`, `changed_file_retention`,
`graph_build_ms`, and `peak_rss_bytes`.

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

`eval/workflows/leave_one_out.py` reuses ContextBench instances but treats
them as a robustness probe: hide one patch file, ask diffctx to recover
it from the remaining context.

## Scripts

### `eval/cli.py` — CLI dispatcher

```bash
python -m eval <subcommand> [args]
```

Selected subcommands (see `SUBCOMMANDS` in `eval/cli.py` for the full table):

| Subcommand | Routes to |
|---|---|
| `cb` | `workflows/contextbench.py` (or `workflows/forensic.py` if `--forensic`) |
| `loo` | `workflows/leave_one_out.py` |
| `compare` | `analysis/compare_runs.py` |
| `curve` | `analysis/budget_curve.py` |
| `aggregate` | `analysis/aggregate_seeds.py` |

### `workflows/contextbench.py` — main evaluation

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
| `--scoring` | `ego` | `ppr` / `ego` / `bm25` |
| `--baseline` | `diffctx` | `diffctx` / `patch_files` / `bm25` |
| `--dataset` | `full` | `full` / `verified` |
| `--tau` | 0.08 | Selection stopping threshold |

**Metrics**: `file_recall`, `file_precision`, `nontrivial_file_recall`,
`line_recall`, `line_recall_nontrivial`, `elapsed_s`, `fragment_count`.
Bootstrap 95% CI on each. Per-language and per-repo breakdowns.

**Output**: `results/cb_{scoring}_n{limit}_b{budget}[_s{seed}].json`
plus stdout tables. One file per seed when `--seeds` lists multiple.

### `workflows/forensic.py` — diagnostic mode

**Purpose**: trace through pipeline stages on individual instances,
classify why each nontrivial gold file was missed (universe →
fragmented → candidate → selected).

**Invocation**: `python -m eval cb --forensic --limit 5`

**Output**:

- Stdout: per-instance trace + stage-wise breakdown table + alerts on
  `patch_coverage < 0.95`.
- `/tmp/diffctx_dump/`: `universe.txt`, `fragmented.txt`, `selected.txt`,
  `candidates.txt`, `diffctx_scores.jsonl` (set via env vars
  `DIFFCTX_DUMP_DIR`, `DIFFCTX_DUMP_SCORES`).

**When to use**: a recall regression appears in the main eval; pick a
failing instance ID and run forensic to see at which pipeline stage
the gold file disappeared.

### `workflows/leave_one_out.py` — leave-one-out robustness

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

### `analysis/budget_curve.py` — budget × mode sweep

**Purpose**: how does recall scale with token budget?

**Args**: `--limit` (default 50).

**Sweep**:

- Budgets: `[8000, 16000, 32000, 64000, 999999]`
- Modes: `[ego, ppr]`

**Workflow**: spawns one `python -m eval cb` subprocess per
(budget, mode) pair, skips configs whose result file already exists,
then aggregates into one curve.

**Output**: `results/curve.json` with per-mode list of
`{budget, n, nontrivial_file_recall, file_recall, line_recall}`.

### `analysis/aggregate_seeds.py` — multi-seed mean ± std

**Purpose**: combine per-seed JSON files into cross-seed statistics.

**Invocation**: `python -m eval aggregate file1.json file2.json …`

**Metrics aggregated**: `file_recall`, `file_precision`,
`nontrivial_file_recall`, `line_recall`, `line_recall_nontrivial`.

**Output**: stdout — per-seed line + cross-seed `mean ± stdev` row.

### `analysis/compare_runs.py` — paired A/B test

**Purpose**: statistical test between two result sets (e.g. before/after
a tuning change).

**Invocation**: `python -m eval compare <after.json> <before.json>`

**Statistics**:

- Bootstrap 95% CI per metric per group (n_iter=10000).
- Paired bootstrap delta with p-value.
- Wilcoxon signed-rank test (`scipy.stats.wilcoxon`).

**Output**: stdout table —
`metric | before CI | after CI | delta CI | p_boot | p_wilc`.

### `harness/stats.py` — statistics helpers

Library, not a script. Used by `analysis/compare_runs.py` and others.

| Function | Returns |
|---|---|
| `bootstrap_ci(values, n_iter=10000, alpha=0.05, seed=42)` | `(mean, lo, hi)` |
| `paired_bootstrap_delta(before, after, n_iter=10000, seed=42)` | `{delta_mean, ci_lo, ci_hi, p_value}` |
| `wilcoxon_paired(before, after)` | `{statistic, p_value}` |

### `harness/common.py` — shared utilities

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

## Sweep orchestration (current)

The full sweep runs via `.github/workflows/eval-sweep.yml`
(`workflow_dispatch`; `mode=smoke` is a 4-cell hosted-runner sanity pass,
`mode=full` provisions a Hetzner host with self-hosted runners). Each matrix
cell calls `python -m eval run-final` (multi-budget reuse via
`--budgets`, EGO depth axis via `--depths`, ablations via `--extra-env` /
`--scoring` / `--tau`), computes per-cell summaries with
`eval.analysis.cell_metrics`, and the aggregate job merges everything with
`eval.analysis.aggregate_sweep`. Run provenance for published sweeps lives in
`results/sweep/README.md`.

### `scripts/sensitivity_check.sh` — parameter sensitivity

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
├── final/v1/                                  # final eval outputs (paper tables)
├── sweep/run_<gh_run_id>/                     # per-cell sweep artifacts + provenance
│   └── cell-<method>-b<budget>-L<depth>-<test_set>/
├── cb_{scoring}_n{limit}_b{budget}.json       # contextbench_diffctx outputs
└── loo_{scoring}_n{limit}_b{budget}.json      # leave-one-out
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
  for `tiktoken-rs` in `crates/diffctx-native/Cargo.toml`. Drift snapshot in
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
| Cargo deps | root `Cargo.lock` (committed workspace lockfile) |
| Python deps | `requirements-eval.lock` from `uv pip compile`; install with `pip install --require-hashes -r requirements-eval.lock` (committed) |
| HuggingFace datasets | `datasets/external-revisions.json` from `python -m eval pin-revisions` (committed) |
| tiktoken BPE | Python `tiktoken` pinned via `requirements-eval.lock` (currently 0.13.0); Rust `tiktoken-rs = "=0.12.0"` in `Cargo.toml`; drift snapshot test `test_tiktoken_o200k_base_encoding_is_pinned` |
| Build determinism | NOT bit-for-bit — Rust release builds carry HashMap ordering non-determinism per `cargo#16693`. Documented limitation; reviewers don't ask for byte-identical `.so`. |

## Multi-benchmark adapter layer

`eval/harness/adapters/` normalizes heterogeneous benchmark sources behind a
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
to verify field mapping without HF fetches (`tests/eval/test_adapters.py`).

**Universal evaluator**:

```python
from eval.harness.adapters import UniversalEvaluator, SelectionOutput

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
python -m eval pin-revisions   # writes datasets/external-revisions.json
git add datasets/external-revisions.json
git commit -m "chore(eval): pin dataset revisions for v1"
```

Resolution order in `adapters/dataset_pins.py::resolve_revision`:

1. `BENCH_REVISION_<HF_PATH_UPPER_SAFE>` env var (one-off override)
2. `datasets/external-revisions.json` (committed pin file)
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
All phases dispatch through `python -m eval` and read manifests from
`datasets/eval-splits/v1/` produced by `eval/datasets/build_splits.py`.

| Phase | Script | Input | Output |
|---|---|---|---|
| Build splits | `python -m eval build-splits` | adapters | `manifests/v1/{calibration,validation,test_*}.txt` + `SPLIT_REPORT.md` |
| One-off run | `python -m eval run --manifest M --tau X --core-budget-fraction Y --out R.json` | manifest | per-instance results JSON |
| 2D grid sweep | `python -m eval calibrate --manifest calibration.txt --tau 0.04,0.08,0.12,0.16 --core-budget-fraction 0.5,0.6,0.7,0.8 --out results/calibration/v1` | calibration manifest | `grid_results.json`, `top_candidates.json`, `grid_report.md` |
| Validation pass | `python -m eval select-final --candidates top_candidates.json --manifest validation.txt --out final_choice.json` | top-K candidates + validation manifest | `final_choice.json` (winner + per-benchmark scores) |
| Final eval | `python -m eval run-final --winner final_choice.json --manifests-dir manifests/v1 --out results/final/v1` | winner + every test manifest | per-test-set JSONs + `PAPER_TABLE.md` |

The objective at sweep time is `min(per_benchmark file_recall)` —
generalization-friendly. Tie-breaking on `top_k_trials` prefers the trial
with lower mean tokens (cheaper context wins under equal recall).

`eval/harness/diffctx_eval_fn.py` is the only file that bridges the adapter
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

Concrete instance manifests live under `datasets/eval-splits/` (when
present); both files are committed to git so the split is reproducible
across runs and reviewers. The `verified` config is the
ContextBench-team-curated holdout — using it as eval keeps the split
under external authority rather than our own choice.

For the 1D `tau` calibration specifically: 30–50 stratified instances
from the calibration pool are sufficient (BO converges fast on a 1D
continuous space). Verify on the full calibration set after best `tau`
is found, then evaluate ONCE on the held-out `verified` set.
