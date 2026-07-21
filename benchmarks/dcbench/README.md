# dcbench — first-party diff-context benchmark

Why a first-party benchmark: of the three public test sets, SWE-bench Verified
and PolyBench-500 carry zero gold files outside the input diff — their recall
measures changed-file retention, not retrieval (paper v2, Section "Trivial
Retention vs. Context Retrieval"). Public sets stay reserved for public
evaluation; dcbench exists for development, calibration, and ablation without
contaminating them.

## Scope

dcbench is **real commits from real repositories with human annotations** —
nothing synthetic. The synthetic YAML cases under `tests/cases/diff/` remain
what they are (a regression test suite); they do not reproduce scale effects
(near-dense graphs, monorepo noise, generated-code blast radius) and are
explicitly out of scope here. v0.1 = 109 curated commits over the
react-native/gitpod/sentry corpus; the expansion pool is the 26 pinned repos
of `repos.yaml` across 15+ languages.

## Reproducibility contract

- `repos.yaml` pins every source repository: `origin` URL + the exact SHA the
  local `test-repos/` checkout is at. Mirrors on Forgejo guard against
  upstream disappearance (`mirror:` field, filled as mirrors are created).
- Every instance stores its diff as a **byte-exact** `patch.diff`
  (`git format-patch -1 --stdout`, no whitespace normalization — see the
  CRLF apply-failure class in the public datasets). Verification: checkout
  `base_commit`, `git apply --index patch.diff` must succeed; a failing
  instance is broken and CI must say so.
- Annotations are plain YAML next to the patch; the whole benchmark is a git
  tree, versioned and diffable.

## Instance layout

```text
benchmarks/dcbench/instances/<repo>__<shortsha>/
  patch.diff        # what changed (byte-exact)
  annotation.yaml   # what is needed to understand it
```

`annotation.yaml`:

```yaml
repo: react-native            # key into repos.yaml
base_commit: <parent sha>     # state BEFORE the change
commit: <full sha>            # the annotated commit
gold:
  - path: packages/...
    tier: essential | helpful
    role: definition | caller | test | config | doc | cochange | unspecified
    in_diff: true|false       # computed, not hand-set
forbidden:                    # files that must NOT appear (noise markers)
  - path: ...
nontrivial_gold_count: N      # gold files with in_diff: false
annotator: legacy-review-2026-06 | human/<name> | llm-verified/<model>
candidates_from: [review]     # generators shown to the annotator (bias guard)
notes: free text
```

## Annotation rules

1. **Every new instance must have `nontrivial_gold_count >= 1`.** An instance
   whose gold coincides with its diff measures retention and is rejected at
   annotation time. (Legacy v0.1 imports are exempt but flagged.)
2. **Candidate lists shown to annotators must come from multiple generators**
   (co-change history, BM25, dependency graph, random distractors) — a
   single-generator candidate list biases gold toward that generator's edges.
3. Roles and tiers are mandatory for new annotations: role-stratified gold is
   what per-edge-category weight calibration consumes.
4. Patches are never normalized; annotation text is English.

## v0.1 provenance

The 109 tier-R instances are converted by `convert_legacy_labels.py` from
`benchmarks/real_world_diff_bench/gold_labels.json` (10-reviewer
should_include/should_not_include labels over the react-native/gitpod/sentry
corpus of `test-repos/TOANALYZE.md`). Legacy labels carry
`role: unspecified`, `tier: essential`, `annotator: legacy-review-2026-06`;
re-annotation with roles is incremental follow-up work. The ~120 hand-curated
commit notes in `test-repos/AlekseiNikiforovIBM/` (mostly pytorch) are the
next expansion pool.

## Split policy

dcbench is a development set: calibrate freely. If dcbench numbers are ever
published, freeze a train/test split first (same discipline as
`benchmarks/manifests/v1/SPLIT_REPORT.md`) and stop calibrating on the test
half from that commit on.
