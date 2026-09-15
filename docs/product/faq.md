# Frequently asked questions

The questions that come back in reviews, issues and first-visit probes,
answered once. Where an answer has a number, the number is in
[BENCHMARKS.md](../../BENCHMARKS.md).

## Is the selection an oracle, or a heuristic?

A heuristic, and the paper says so. Relevance is a hand-constructed estimator
over a typed dependency graph: call, import, type and symbol edges from
tree-sitter parses plus intra-scope name matching, not data-flow analysis.
It degrades with metaprogramming and dynamic dispatch, more in dynamic
languages than in annotated static ones. What is guaranteed is the selection
step: lazy-greedy budgeted maximisation of a monotone submodular objective
under a partition matroid, so given the scores, the packing is near-optimal.
The scores themselves are measured, not proven — pooled file recall 0.919 at
8000 tokens over 1500 instances, with the decomposition that shows where the
headline number comes from.

## Do I need the `tree-sitter` extra?

No. There is no such extra any more. Every grammar diffctx uses is compiled
into the Rust engine that ships inside the wheel; the Python package imports
no tree-sitter module. `pip install diffctx` is the whole install.

## Whose tokens does `--budget` count?

tiktoken's `o200k_base` (the GPT-4o / GPT-4.1 encoder), exactly, on the whole
rendered document. Claude, Gemini and Llama tokenize differently, typically
within single-to-low-double-digit percent. Leave headroom, or set
`DIFFCTX_TOKEN_SAFETY_FACTOR` to inflate every count by a factor you measured
against your model. Details, including why there is no `--tokenizer` flag, in
[Token counting](token-budget.md).

## Is this just a prompt builder?

It is a context *selector* with three interfaces. The CLI writes a document;
the Python API returns the same artifact as a dict; the MCP server exposes it
as one tool (`diffctx_context`) that an agent calls with a repository path and
a diff range and gets the selected fragments back — `mode=locate` for a ranked
list without bodies, `fragment_ids` to fetch specific ones. The artifact is
schema-versioned (`diffctx.context.v1`, JSON Schema in `schemas/`) and carries
provenance (engine version, effective-configuration hash, tokenizer), so a
consumer can tell two runs apart without diffing their text.

## What happens on a monorepo?

The run is bounded and says what bounded it. `--timeout` (default 300 s) is a
cooperative wall-clock deadline; byte, candidate-file, edge-contribution and
need caps bound memory. A run that hits any of them still produces a valid
artifact, marked `coverage.status: partial` with the limit reasons named. The
three instances that used to take 40–100 s and 5–11 GB (home-assistant,
polars, kubernetes) run in 4–60 s and under 3.5 GB after the graph builders
were bounded; the remaining cost on the largest is parsing 145 MB of generated
Go, not selection.

## Why is the raw diff not in the output by default?

Because the point is what the diff does *not* show: the callers, types and
configuration the changed lines depend on. `--with-raw-diff` bundles git's own
patch ahead of the selected context when the consumer wants both; it is not
charged to `--budget` (the stderr summary reports the real size) and secret-
like, ignored and lock-file sections stay out of it.

## Can it leak secrets?

It reads the repository, so it can emit what the repository contains. Two
floors apply everywhere: secret-by-name files (`id_rsa`, `*.pem`, `.env`-style
paths your ignore rules cover) are withheld from every read surface, and a
last pass replaces credential-shaped strings (cloud and API keys, tokens,
JWTs, PEM blocks) with `[REDACTED:<category>]`, reporting the count in the
artifact. Neither is a guarantee; [SECURITY.md](../../SECURITY.md) states
exactly what is and is not caught.

## Is the output deterministic?

Yes. Same repository state, same range, same configuration, same artifact.
No model is called, no network is used, nothing persists between runs. The
`provenance` block carries the effective-configuration hash so an unexpected
difference can be traced to a parameter rather than guessed at.
