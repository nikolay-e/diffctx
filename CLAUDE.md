# TreeMapper

<!-- Extends ../CLAUDE.md -->

## Ultimate Goal

**CRITICAL: This is the guiding star of the entire project.
Every feature, every design decision, every line of code must
serve this goal. It is an asymptotic ideal — not a finish line
to cross, but a direction to relentlessly pursue.**

**Maximize the speed and depth of understanding textual
information — for any reader, in any scenario.**

Whether the consumer is an LLM processing a context window or a
human reviewing a code change, TreeMapper's job is the same:
extract the maximum signal from a codebase and present it in the
clearest, most information-dense form possible. Every design
decision optimizes for **comprehension-per-token** — the ratio
of understanding gained to attention spent. This metric is the
single lens through which all trade-offs are evaluated.

---

## Two Modes of Operation

**Tree Mapping Mode** (`treemapper .`) — Filesystem-focused.
Walks the directory tree respecting hierarchical ignore patterns,
reads file contents with binary/encoding detection, and serializes
to YAML/JSON/text/Markdown. Deterministic, side-effect-free.

**Diff Context Mode** (`treemapper . --diff`) — Semantics-focused.
Analyzes a git diff to intelligently select the minimal set of
code fragments needed to understand a change. For the formal
theoretical foundation, see the research paper:
[Context-Selection for Git Diff][paper].

[paper]: https://nikolay-eremeev.com/blog/context-selection-git-diff/

## Development

```bash
pip install -e ".[dev,tree-sitter]"
pytest
pre-commit run --all-files
```

## Testing

Integration tests only — test against real filesystem and real git
repos. No mocking.

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
| Output      | YAML              | LLM-readable, literal blocks |
| Tokens      | tiktoken o200k    | GPT-4o standard, exact BPE   |
| Ignores     | pathspec          | gitignore-compatible         |
| Parsing     | tree-sitter       | 10 languages, AST-level      |
| Ranking     | PPR               | Relevance with natural decay |
| Selection   | Lazy greedy       | Near-optimal, linear time    |
| Git         | subprocess UTF-8  | Platform-safe, non-ASCII     |
| Diff        | git diff unified=0| Exact line ranges            |

## License

Apache 2.0
