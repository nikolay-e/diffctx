# diffctx

<!-- Extends ../CLAUDE.md -->

## Ultimate Goal

**CRITICAL: This is the guiding star of the entire project.
Every feature, every design decision, every line of code must
serve this goal. It is an asymptotic ideal — not a finish line
to cross, but a direction to relentlessly pursue.**

**Maximize the speed and depth of understanding textual
information — for any reader, in any scenario.**

Whether the consumer is an LLM processing a context window or a
human reviewing a code change, diffctx's job is the same:
extract the maximum signal from a codebase and present it in the
clearest, most information-dense form possible. Every design
decision optimizes for **comprehension-per-token** — the ratio
of understanding gained to attention spent. This metric is the
single lens through which all trade-offs are evaluated.

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

```bash
pip install "maturin>=1.10,<1.11"
pip install -e ".[dev,full,mcp]" --no-build-isolation
pytest
pre-commit run --all-files
```

Rust core: `cd crates/diffctx-native && cargo test --lib` (CI runs the
YAML suite on a 20-case sample via `DIFFCTX_YAML_CASES_LIMIT=20`; the
full suite carries a known score-threshold failure baseline gated
nightly). Full setup: [CONTRIBUTING.md](CONTRIBUTING.md).

## Performance-change discipline (E/Q classes)

Every change is either **E-class** (bit-equivalent: identical selection
output on identical input) or **Q-class** (output-changing). E-class
changes may land any time but must pass
`python -m eval equivalence --a <old-run> --b <new-run>` on the
stratified 40-instance sample (`results/sweep_v2_local/equiv/manifests`),
plus a double-run determinism check. Q-class changes are frozen during an
evaluation cycle (calibration -> validation -> sweep) — they invalidate
the calibration and force a full rerun. The shared token corpus, the
per-blob token cache, and the two-pass edge cap are all E-class precedents;
fragmentation or scoring changes are Q-class.

## Testing

The Python suite is integration-only — real filesystem, real git
repos, no mocking. The Rust crate additionally carries inline
`#[test]` units run via `cargo test --lib`.

The diff context tests use a **YAML-based declarative framework**:
each test case defines initial files, changed files, and expected
output assertions. A dedicated test runner creates a real git repo
per test, commits the files, runs the full diffctx pipeline, and
verifies results.

**Negative testing via garbage injection**: every test case
automatically includes ~10 unrelated "garbage" files with
distinctive markers. Tests verify the algorithm excludes this
noise, catching regressions in relevance filtering. Each garbage
file uses unique prefixed identifiers (e.g. `GARBAGE_*`) so leaks
are unambiguously detectable.

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
