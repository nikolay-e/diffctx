# How diffctx compares

diffctx is **diff-seeded**: the input is a change, the output is the
surrounding code needed to understand it, packed under a hard token
budget. Nothing is indexed ahead of time and nothing persists between
runs. That is a different category from the two families it is most
often confused with:

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

## When to use something else

**Use a persistent whole-repo code-graph server** when the questions are
repository-wide and repeated rather than change-scoped: tracing all
callers of a symbol, impact analysis before a refactor, interactive
exploration on a warm index. diffctx builds its graph per invocation and
discards it; it is not a code-navigation service.

**Use a whole-repository packer** ([repomix](https://repomix.com/) and
friends) when you actually want the whole codebase in the prompt and
selection buys nothing.

**Use diffctx** when a change already exists and the question is what
else must be read to understand it, under a token budget you control.
