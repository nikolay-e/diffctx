# How diffctx compares

diffctx is **diff-seeded**. The input is a change — a commit, a branch
range, or an uncommitted working tree — and the output is the surrounding
code needed to understand that change, packed under a hard token budget.
Nothing is indexed ahead of time and nothing persists between runs.

That is a different category from the two families of tools it is most
often confused with:

- **Whole-repository packers** — [repomix](https://repomix.com/), `tree`,
  and the various code-to-prompt utilities. The seed is the repository.
  The output is (a filtered view of) everything.
- **Query-seeded persistent code-graph MCP servers** — for example
  code-review-graph or Kodus Graph. They build and maintain a whole-repo
  graph and answer structural questions against it: who calls `X`, what
  breaks if `Y` changes.

All three are useful. They answer different questions.

## Comparison

| | Whole-repo packers | Code-graph MCP servers | **diffctx** |
|---|---|---|---|
| Seed | the repository | your query | **a diff** |
| Output | full/filtered codebase export | answers about the graph | **fragments explaining the change** |
| Question it answers | "give the model my repo" | "who calls `X`?" | "what must I read to review this patch?" |
| Persistent index | no | yes (maintained, kept warm) | no (built per run, thrown away) |
| Hard token budget | usually not | n/a | yes, enforced |
| Model calls / API keys | no | varies | none |
| Deterministic output | yes | varies | yes |
| Interface | CLI | MCP | CLI, Python API, MCP |

## What has actually been measured

The published evaluation
([DOI 10.5281/zenodo.18824579](https://doi.org/10.5281/zenodo.18824579))
scores file-level recall against annotated golden contexts at a fixed
budget, over 1500 instances from SWE-bench Verified, PolyBench-500, and
ContextBench Verified:

- Pooled file recall **0.919** [95% CI 0.908, 0.929] at an 8000-token
  budget.
- Paired both-OK deltas against same-budget external baselines:
  **+0.371** over whole-file BM25 packing and **+0.410** over the Aider
  repo-map oracle-mentioned upper bound (permutation *p* = 1e-5).

The limitation the paper states about its own headline number matters as
much as the number:

- That 0.919 is **substantially changed-file retention** — keeping the
  files the patch touches. Only ContextBench Verified carries gold files
  beyond the input diff, so genuine retrieval is measured on a
  213-instance subset.
- On that subset the internal BM25 lexical mode is the strongest single
  signal at **0.402** nontrivial recall, ahead of the **0.352** of the
  deployed graph-lexical default. The deployed default is not the best
  measured retriever; combining the two signals in the scorer is the
  open work item.

Read that as: the packing and the budget discipline are well supported by
measurement, and the graph scoring still has measured headroom.

The properties that hold regardless of the scoring debate, and that are
verifiable by running it: **deterministic** output for identical input,
**runs entirely locally**, **no model calls and no API keys**, a **hard
token budget**, and an **MCP server** for editor and agent integration.

## When to use something else

**Use a persistent whole-repo code-graph server** (code-review-graph,
Kodus Graph, and similar) when the questions are repository-wide and
repeated rather than change-scoped: tracing all callers of a symbol,
impact analysis before a refactor, or interactive exploration where you
want a warm index instead of a fresh per-run graph. diffctx builds its
graph per invocation and discards it; it is not a code-navigation
service and does not try to be one.

**Use a whole-repository packer** (repomix and friends) when you actually
want the whole codebase in the prompt — onboarding a model to an
unfamiliar small repo, one-shot "read all of this" tasks, or when the
repo fits comfortably in the context window and selection buys nothing.

**Use diffctx** when a change already exists and the question is what
else must be read to understand it, under a token budget you control.

For a per-flag description of that behaviour see the
[README](README.md); for the formal problem statement and the full
evaluation see the [paper](https://doi.org/10.5281/zenodo.18824579).
