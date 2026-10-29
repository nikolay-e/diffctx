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

## External benchmarks (after v2)

Four datasets that live outside the sweep matrix, run by `eval-sweep.yml` with
`mode=external` on free GitHub-hosted runners, one engine build per run. The
numbers below are the base: engine `c72bdc8e`. The after column is filled when
the typed-link work is measured on the same datasets; nothing here is a claim
about it.

| Dataset | Base run | After run |
|---|---|---|
| Agent Retrieval Bench `edit2ripple` | [40734249322](https://github.com/nikolay-e/diffctx/actions/runs/40734249322), baselines [40737747910](https://github.com/nikolay-e/diffctx/actions/runs/40737747910) | *to fill* |
| ContextBench Verified, nontrivial | [40734249322](https://github.com/nikolay-e/diffctx/actions/runs/40734249322) | *to fill* |
| SWE-rebench V2 infrastructure slice | [40739040824](https://github.com/nikolay-e/diffctx/actions/runs/40739040824) | *to fill* |
| CfgNet-500 | [40740458207](https://github.com/nikolay-e/diffctx/actions/runs/40740458207) | *to fill* |

### Agent Retrieval Bench `edit2ripple`

Given an anchored file change, find the other files the change affects (58
samples, 10 repositories, release `v2_edit2ripple`). Scored by the benchmark's
own evaluator at a pinned commit. **Seeding is diff-only**: diffctx receives the
anchor file's diff and nothing else, so the intent text the published numbers
use is dropped from every system below. 3 of the 58 anchor diffs are truncated
beyond use and do not apply, leaving 55 scored samples.

| Over the 55 samples, diff-only, anchor excluded | Recall@5 | Recall@10 | Recall@20 | MRR | BCY@8k | After (Recall@20) |
|---|---|---|---|---|---|---|
| RepoMap (benchmark's) | 0.441 | 0.508 | 0.603 | 0.363 | 0.436 | n/a |
| lexical (benchmark's) | 0.403 | 0.500 | 0.556 | 0.411 | 0.409 | n/a |
| BM25 (benchmark's) | 0.286 | 0.361 | 0.509 | 0.266 | 0.323 | n/a |
| diffctx, `locate` at 8k, shipped defaults | 0.276 | 0.312 | 0.395 | 0.256 | 0.259 | *to fill* |

diffctx's own 8k selection holds 0.414 of the gold files (17.6 files per
sample). The three baselines, rerun with the intent, score at most 0.04
Recall@20 higher, and lexical and BM25 reproduce the summaries the release ships
exactly (RepoMap within 0.004), so the gap is not the missing intent. It is a
code-only gap tracked in #688.

- 33 of the 58 shipped anchor diffs do not apply as stored (a trimmed trailing
  context line, or a cut at a size limit marked `...[truncated]`). The adapter
  rewrites the hunk counts, drops the marker and leaves out a hunk cut before its
  first change. 27 of the 55 scored seeds are truncated, so they are partial
  changes; on the 24 untouched seeds diffctx reaches Recall@20 0.472, MRR 0.379.
- Gold is the other files changed in the same fix: a recall oracle for
  co-change, weak evidence for precision. 55 samples in 10 repositories support
  description, not a confirmatory test.
- BCY packs with the evaluator's tokenizer, not `o200k_base`; it is never mixed
  with the engine's own token counts.

### ContextBench Verified, nontrivial

The Verified instances whose gold names a file the patch does not touch, run
through the sweep's own machinery (ego, 8000 tokens, shipped defaults). 218
instances by this definition; 4 Java instances have a base commit that is no
longer reachable upstream, so 214 are scored.

| | Base | After |
|---|---|---|
| File recall (214 scored) | 0.730 [0.697, 0.762] | *to fill* |
| Nontrivial-file recall | 0.491 [0.436, 0.546] | *to fill* |
| Changed-file retention | 0.993 | *to fill* |
| Line recall | 0.295 | *to fill* |

This is the code-only regression check for the typed-link work. It is not
comparable to the 0.352 in the table above: that figure is from an earlier
engine and may use a different instance set (213 there, 218 here).

### SWE-rebench V2 infrastructure slice

Instances whose fix changes at least one code file and one infrastructure file
(Dockerfile, compose, Kubernetes, Helm, Terraform, CI workflow, env template);
200 drawn from 1215 qualifying across 147 repositories, 400 rows. The seed is one
side's hunks applied as the change; the other side's modified files are the
hidden gold. Selection recall is the share of hidden files in the 8k selection;
ranked recall ranks every scored file by best fragment relevance, seeds excluded.

| Direction (seed to hidden) | Recall | R@5 | R@10 | R@20 | After |
|---|---|---|---|---|---|
| code to infra | 0.018 | 0.068 | 0.091 | 0.121 | *to fill* |
| infra to code | 0.049 | 0.127 | 0.200 | 0.313 | *to fill* |

Thirty instances were labelled by hand before any diffctx run, file by file, as
coupled to the seed side (the other side's hunks name the same key, path or
symbol) or an unrelated co-change. Recall on those 30, all hidden files: 0.000
(code to infra), 0.072 (infra to code). Against the coupled files only (20
instances each way have one): 0.000 and 0.185. The gold is a co-change record
from one fix, so a hidden file that is a version bump or a drive-by edit counts
as missed; the labelled subset exists to say how much of the recall gap that
explains.

### CfgNet-500

The 500 labelled configuration-dependency candidates (199 true, 301 false) of
ten Spring Boot projects from the CfgNet methodology study (arXiv 2410.08801), at
the project versions in that paper. The labels are fetched at run time from a
pinned commit of the authors' repository and checked against a pinned digest;
nothing of theirs is stored here. A candidate is decided when its two options sit
in two distinct files the file-level graph contains; it is predicted positive
when the graph joins those files through a `config`, `config_generic` or
`key_match` edge.

| | Base | After |
|---|---|---|
| Decided (of 500) | 404 | *to fill* |
| Predicted positive | 1 (true) | *to fill* |
| Recall of all 199 true | 0.005 | *to fill* |
| Any edge category joins the pair: precision | 0.413 [0.345, 0.484] | *to fill* |
| Any edge category: recall of all true | 0.392 | *to fill* |

That 40% of the candidates are true means a rule that predicts "linked" for every
pair is 40% precise; the any-edge row is the structural-proximity floor, not a
config measurement.

### Common caveats

- Hosted-runner wall times vary from run to run (the same engine measured
  1.1x total, 1.2x median apart on ContextBench); read a timing ratio against
  `python -m eval external-timing` on two runs of one engine first.
- The ARB corpus snapshots stay under their upstream licences and are fetched
  from the pinned release at run time; the SWE-rebench V2 manifest holds
  identifiers and our labels only.
- Each run records the engine version and the effective-configuration hash, so
  two rows compare only when those match.

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
