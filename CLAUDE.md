# diffctx

<!-- Extends ../CLAUDE.md -->

## Ultimate Goal

**Maximize the speed and depth of understanding textual
information — for any reader, in any scenario.**

Whether the consumer is an LLM processing a context window or a
human reviewing a code change, diffctx's job is the same:
extract the maximum signal from a codebase and present it in the
clearest, most information-dense form possible. The single lens
for every trade-off is **comprehension-per-token** — the ratio
of understanding gained to attention spent.

---

## Two Modes of Operation

**Tree Mapping Mode** (`diffctx .`) — Filesystem-focused.
Walks the directory tree respecting hierarchical ignore patterns,
reads file contents with binary/encoding detection, and serializes
to YAML/JSON/text/Markdown. Deterministic, side-effect-free.

**Diff Context Mode** (`diffctx . --diff`) — Semantics-focused.
Analyzes a git diff to intelligently select the minimal set of
code fragments needed to understand a change. For the formal
theoretical foundation, see the research paper
([source in `paper/v2/`, snapshot at git tag `paper-v2`][paper]).

[paper]: https://doi.org/10.5281/zenodo.18824579

## Development

Setup, test commands, and the per-case YAML-corpus CI gate
(`crates/diffctx-native/tests/known_below_threshold.txt`, bidirectional:
a listed case that starts passing also fails) live in
[CONTRIBUTING.md](CONTRIBUTING.md). Nightly CI reruns the corpus with
`DIFFCTX_YAML_IGNORE_BASELINE=1`, so baseline growth/shrinkage is
tracked even when every per-commit verdict passes.

## Performance-change discipline (E/Q classes)

Every change is either **E-class** (bit-equivalent: identical selection
output on identical input) or **Q-class** (output-changing). Q-class
changes are frozen during an evaluation cycle (calibration -> validation
-> sweep) — they invalidate the calibration and force a full rerun. The
shared token corpus, the per-blob token cache, and the two-pass edge cap
are all E-class precedents; fragmentation or scoring changes are Q-class.

**Proving E-class, per commit: `scripts/bitcheck.sh record` on the
pre-change build, then `check` after.** 24 cells against a worktree pinned
at a fixed SHA, so editing the code cannot edit the input. It drives the
**native binary**, so it says nothing about `writer.py`, `_native/` or
`mcp/` — a Python-side E-class claim needs its own byte-diff through the
installed wheel. Its complement is the full corpus
(`cargo test --release --test yaml_cases`), which is the
only net over the per-language edge and parser tables bitcheck never
reaches.

**At a release cycle:** `python -m eval equivalence --a <old-run> --b
<new-run>` on the stratified 40-instance sample
(`results/sweep_v2_local/equiv/manifests`) plus a double-run determinism
check. This one needs two full eval output directories and a local
`results/` tree, so it is not a per-commit gate and no CI job runs it.

## Testing

Suite shape and commands: [CONTRIBUTING.md](CONTRIBUTING.md). Each YAML
case runs the full pipeline against a real git repo built per test, and
**every case automatically injects ~10 unrelated "garbage" files**
(`tests/garbage_data.py`) with unique `GARBAGE_*` markers — asserting
they stay excluded is what catches relevance-filter regressions, and
the prefixes make any leak unambiguous.

**Reading oracle results** — two traps that make raw numbers lie:

- `changed_files` in a case YAML is the **final state of every file**,
  not the diff. Usually only one differs from `initial_files`; the rest
  produce no hunks and are legitimately forbidden. A path appearing
  under `changed_files` is *not* evidence the tool must emit it.
- `forbidden_rate = hit_forbidden / forbidden_total` **saturates**. The
  forbidden list carries a path entry plus several anchor-only entries
  for the same file, so one wrong file trips ~5 entries and drives
  `score = recall * (1 - forbidden_rate)` to zero. `forbidden_rate=100%`
  means "at least one wrong file", not "emitted all the garbage" — the
  real magnitude is a median of 1 extra file.

The harness passes `DEFAULT_PPR_ALPHA` / `DEFAULT_STOPPING_THRESHOLD`
so the gate measures the shipped configuration; it once passed
`tau=0.0`, which hid 119 cases in both directions (#175). Never
replace those with literals.

## Technology Choices

| Decision    | Choice            | Rationale                    |
|-------------|-------------------|------------------------------|
| Output      | MD default; YAML/JSON/txt | MD ~7% cheaper on real diffs (#104) |
| Tokens      | tiktoken o200k    | GPT-4o standard, exact BPE   |
| Ignores     | pathspec          | gitignore-compatible         |
| Parsing     | tree-sitter       | 30+ languages, AST-level     |
| Ranking     | ego (default), PPR, BM25 | Relevance with natural decay |
| Selection   | Lazy greedy       | Near-optimal, linear time    |
| Git         | subprocess UTF-8  | Platform-safe, non-ASCII     |
| Diff        | git diff unified=0| Exact line ranges            |
