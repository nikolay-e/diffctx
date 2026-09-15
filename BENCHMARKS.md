# Benchmarks

The measured results behind diffctx, on one page, with what each number was
measured on. Every figure below is taken from the published evaluation
([paper v2, DOI 10.5281/zenodo.18824579](https://doi.org/10.5281/zenodo.18824579);
source in `paper/v2/src/main.tex`), which in turn traces every table value to
a committed per-instance CSV. Nothing here is re-derived.

## Protocol

- **Task**: post-hoc diff understanding. The gold patch is the *input*; the
  score is whether the selected context covers the files the patch touches
  and the dependency neighbourhood needed to interpret them. Benchmarks are
  repurposed issue-resolution datasets used as sources of known diffs, never
  as targets an agent must generate.
- **Metric**: file-level recall against annotated golden contexts at a fixed
  token budget of 8000 `o200k_base` tokens.
- **Datasets** (1500 instances): SWE-bench Verified, PolyBench-500,
  ContextBench Verified. Consolidated rerun, v4-calibrated.
- **Configuration**: the shipped default — bounded ego-network scoring, the
  discovery ensemble, adaptive stopping at the default threshold.

## Headline recall at 8000 tokens

| Benchmark | File recall |
|---|---|
| SWE-bench Verified | 0.998 |
| PolyBench-500 | 0.942 |
| ContextBench Verified | 0.816 |
| **Pooled (1500 instances)** | **0.919** [95% CI 0.908, 0.929] |

## What the headline number is made of

Recall decomposes into *changed-file retention* (keeping the files the patch
touches) and *genuine retrieval* (gold files beyond the input diff). Only
ContextBench Verified carries gold files beyond the diff, so retrieval is
measured on its 213-instance subset:

| Scoring mode | Nontrivial recall (213 instances) |
|---|---|
| Internal BM25 (lexical) | 0.402 |
| Ego network (deployed default) | 0.352 |
| Personalized PageRank | 0.244 |

The deployed default is not the strongest measured retriever on this subset.
The two signals are complementary: a score-free union of the BM25 and ego
selections reaches 0.473 nontrivial and 0.870 headline recall at a measured,
deduplicated mean cost of 6.1k tokens — within the 8000-token budget for
91.5 % of instances. Calibrated in-scorer combination of the two is the
highest-leverage open item, tracked in the v3 milestone.

## Against external baselines

Paired both-OK deltas at the same budget, permutation *p* = 1e-5 throughout:

| Baseline | Δ file recall (diffctx − baseline) | 95% CI |
|---|---|---|
| Whole-file BM25 packing | +0.371 | [+0.350, +0.392] |
| Aider repo-map, fair input | +0.423 | [+0.399, +0.447] |
| Aider repo-map, oracle-mentioned upper bound | +0.410 | [+0.385, +0.434] |

External BM25 does not overtake diffctx at any measured budget in this
snapshot.

## Reproducing

The evaluation harness lives under `eval/`; `eval/README.md` is its operator
map. The final-eval command shape is:

```bash
python -m eval run-final --winner ... --manifests-dir datasets/eval-splits/v1 --out results/...
```

Baselines are `--baseline patch_files | random | aider_fair | aider_oracle`,
scoring modes `--scoring ego | ppr | bm25`. Two full runs of one input are
compared bit-for-bit with `python -m eval equivalence --a <old> --b <new>`,
which is the gate every performance change to the engine passes before a
release. A full sweep runs from `.github/workflows/eval-sweep.yml`
(`workflow_dispatch`, `mode=full`) on an ephemeral cloud host.

## What is not measured here

No head-to-head bake-off against other diff-aware tools has been run; the
comparison above is against packing and repo-map baselines, which is what the
published protocol covers. Tokenizer counts are `o200k_base`; other model
families count differently (see
[Token counting](docs/product/token-budget.md)).
