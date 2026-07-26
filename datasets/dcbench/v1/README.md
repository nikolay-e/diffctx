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
datasets/dcbench/v1/instances/<repo>__<shortsha>/
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
    hop: N                    # graph distance from diff files (computed via graph export)
string_indirection: true|false  # gold linkage runs through a string (DI/event bus/routing)
forbidden:                    # files that must NOT appear (noise markers)
  - path: ...
nontrivial_gold_count: N      # gold files with in_diff: false
annotator: legacy-review-2026-06 | human/<name> | llm-verified/<model>
candidates_from: [review]     # generators shown to the annotator (bias guard)
notes: free text
```

## Annotation rules

1. **Nontrivial gold is the point.** Gold that coincides with the diff
   measures retention; annotators must genuinely hunt context outside the
   diff, and may set `no_nontrivial_context: true` (with justification)
   only for verified self-contained changes — never invent gold to pad.
2. **No single-generator bias.** Annotators must not be limited to what any
   one retrieval signal surfaces. In the executed pipeline this is enforced
   by free exploration (annotators run their own `git grep`/`show` hunts);
   the optional `python -m eval generate-dcbench-candidates` shortlists
   (co-change, BM25, graph,
   random distractors) are hints to judge, not a frame.
3. Roles and tiers are mandatory for new annotations: role-stratified gold is
   what per-edge-category weight calibration consumes.
4. Patches are never normalized; annotation text is English.

## How this benchmark was created (provenance)

1. **Commits: hand-curated.** 109 from `datasets/real-world-diff/v1/commits.tsv`
   (react-native/gitpod/sentry) + 263 hand-audited commits from
   `test-repos/TOANALYZE.md` across 12 coverage repos (every SHA verified,
   selection rationale recorded per commit). Extraction to instances:
   `python -m eval convert-legacy-labels` and
   `python -m eval extract-dcbench-commits`.
2. **Reproducibility layer.** Byte-exact `git format-patch` per instance,
   SHA pins in `repos.yaml`, strict-apply verification
   (`python -m eval verify-dcbench`, index-level `--cached` check; 372/372
   pass).
3. **Gold annotation (2026-07-22).** The 263 coverage instances were
   annotated by ~35 parallel Claude Sonnet 5 reviewer agents following
   `ANNOTATION_PROTOCOL.md`: per instance, the agent reads the patch,
   explores the repository read-only at the pinned commit (`git
   show`/`grep`/`diff-tree` only — no checkouts, shared clones), and writes
   evidence-based gold entries (every entry carries a `rationale` naming the
   linking symbol/mechanism), 2–4 `forbidden` distractors with reasons, and
   the `string_indirection` flag. Agents self-validated YAML and path
   existence; a final batch validator cross-checked schema and
   `nontrivial_gold_count` consistency over all 372 (zero defects at close).
   `annotator: llm/sonnet-5-2026-07-22` marks these.
4. **Legacy import.** The 109 originals carry 10-reviewer
   should_include/should_not_include labels (`annotator:
   legacy-review-2026-06`, `role: unspecified`); role-enrichment and a
   second-reviewer pass over the LLM annotations are follow-up work, as is
   human spot-checking (the accepted QA mode for LLM-labeled sets).
5. **Hop annotation** (`python -m eval annotate-dcbench-hops`) is applied
   where graph builds are
   feasible; blocked on diffctx#116 for the three heavy legacy repos.

Result snapshot at close: 372 instances, 2,999 gold entries, 392 nontrivial
gold files, 201 instances (54%) with nontrivial gold, 114 flagged
string_indirection. The ~120 hand-curated commit notes in
`test-repos/AlekseiNikiforovIBM/` (mostly pytorch) are the next expansion
pool.

## Expansion set (v0.2 targets)

The existing 27 repos stay as the regression anchor. Eleven additions, each
closing at least two uncovered dimensions (grammar-tail edge builders and/or a
depth class no current repo generates):

| Repo | Grammars | Depth class / dimension |
|---|---|---|
| rabbitmq/rabbitmq-server | Erlang, Elixir, Bazel | OTP behaviour → callback → supervisor, 3-hop |
| gradle/gradle | Groovy, Java, Kotlin | buildSrc → convention plugin → build script, 3-hop through build logic |
| ktorio/ktor | Kotlin | Kotlin-primary fixture for the jvm.rs regex → tree-sitter migration |
| metabase/metabase | Clojure, TS | FE/BE polyglot boundary |
| AppFlowy-IO/AppFlowy | Dart, Rust | second FFI type after PyO3 |
| calcom/cal.com | Prisma, TS | codegen boundary (.prisma → client → consumer) |
| hashicorp/terraform-provider-aws | HCL, Go | config edges + Go |
| Perl/perl5 | Perl, C | grammar + legacy scale |
| jgm/pandoc | Haskell | grammar, mono-language isolation |
| tigerbeetle/tigerbeetle | Zig | grammar, exemplary commit hygiene |
| mattermost/mattermost-data-warehouse | dbt, SQL | data-stack dimension (gitlab-data/analytics went private, 401) |

B-tier (only with spare sweep budget): JuliaLang/julia, tidyverse/ggplot2,
nix-community/home-manager, sveltejs/kit. LaTeX/nim/openapi: not worth the
sweep minutes.

All expansion clones use `--filter=tree:0`: co-change works (full commit
graph), but first-diff blob fetches hit the network and wall-clock is not a
perf signal until caches warm. gradle and perl5 have giant histories and are
expected to reproduce the near-dense hang class — run them only with
per-instance timeouts on a dev binary.

## Commit curation rules (per repo, ~25--30 commits)

1. Stratify by diff shape (delete / rename / merge / big / binary), following
   the existing TOANALYZE protocol.
2. **Hop annotation is mandatory**: for every candidate commit, compute the
   dependency-graph distance of each gold file from the diff files (via the
   graph export) and record it as `hop: N` on the gold entry.
   *Status note:* hop annotation of the legacy-109 instances is currently
   blocked by diffctx#116 — the full project graphs of gitpod/sentry/
   react-native exceed practical build time even with
   `DIFFCTX_MAX_EDGES_PER_NODE` lowered (the bottleneck is edge emission,
   which the cap does not reduce). Annotate hops for those after the #116
   fix; lighter coverage repos are unaffected. Select commits
   so that **at least 40\% have gold files at hop >= 2** --- otherwise the set
   is biased to hop-1 neighbors and any depth sweep plateaus because commits
   are shallow, not because depth is useless.
3. Tag commits whose gold linkage runs through a string (DI by name, event
   bus, URL routing, celery-style task names) with `string_indirection: true`
   and report that slice separately --- structural parsers cannot see these
   edges, so mixing them into pooled recall understates everything else.
4. Gold labels via the same multi-reviewer protocol as the 109-commit set;
   re-label / re-run the original react-native/gitpod/sentry trio on a dev
   binary --- the 1.10.2-era summary numbers (hang=72) are dead data.

## Split policy

dcbench is a development set: calibrate freely. If dcbench numbers are ever
published, freeze a train/test split first (same discipline as
`datasets/eval-splits/v1/SPLIT_REPORT.md`) and stop calibrating on the test
half from that commit on.
