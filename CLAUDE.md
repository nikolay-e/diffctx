# TreeMapper

> Extends [../CLAUDE.md](../CLAUDE.md)

## Development

```bash
pip install -e ".[dev]"
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

## Two Modes of Operation

TreeMapper operates in two fundamentally different modes that
share output formatting, token counting, and file reading
infrastructure:

**Tree Mapping Mode** (`treemapper .`) — Filesystem-focused.
Walks the directory tree respecting hierarchical ignore patterns,
reads file contents with binary/encoding detection, and serializes
to YAML/JSON/text/Markdown. Deterministic, side-effect-free.

**Diff Context Mode** (`treemapper . --diff`) — Semantics-focused.
Analyzes a git diff to intelligently select the minimal set of
code fragments needed to understand a change. This is the core
intellectual property of the project — a graph-based relevance
engine described in detail below.

---

## Diff Context: Architecture & Design

### The Problem

When reviewing a code change, the diff alone is rarely sufficient.
A developer needs surrounding context: the function being called,
the interface being implemented, the config driving deployment.
But naively including "everything related" explodes the context
window. The challenge is selecting the **minimal, sufficient
context** within a token budget.

### The Approach: Graph-Based Relevance Propagation

The diffctx engine models a codebase as a **weighted directed
graph** where nodes are semantic code fragments and edges represent
dependencies between them. Changed code seeds the graph, relevance
propagates through edges via Personalized PageRank, and a
budget-aware greedy algorithm selects the best fragments.

This approach was chosen over simpler alternatives (call-graph
depth, grep-based expansion, file-level inclusion) because:

- **Transitive importance decays naturally** — a function calling
  a modified function is relevant; a function calling *that*
  function is less so. PPR captures this without manual depth
  limits.
- **Heterogeneous relationships combine gracefully** — imports,
  type references, config links, test patterns, and lexical
  similarity all contribute edges with different weights. No
  single signal captures all dependencies.
- **Budget optimization is principled** — submodular utility
  maximization with lazy greedy selection gives near-optimal
  coverage per token spent.

### Pipeline Stages

The engine operates as a 7-stage pipeline:

1. **Diff Parsing** — Extract changed file paths and exact line
   ranges from git diff output.

2. **Core Fragment Identification** — Break changed files into
   semantic units (functions, classes, config blocks, doc sections)
   using language-aware parsers, then identify which fragments
   cover the actual changed lines.

3. **Concept Extraction** — Extract identifiers from added/removed
   diff lines. These "diff concepts" represent the vocabulary of
   the change and drive relevance scoring.

4. **Universe Expansion** — Discover related files beyond those
   directly changed. Edge builders scan for imports, config
   references, naming patterns. Rare identifiers (appearing in
   ≤3 files) trigger targeted file discovery.

5. **Graph Construction** — Build fragment-level dependency graph.
   26 edge builders contribute weighted edges across 6 categories
   (see below). Edges are aggregated via max — if any builder
   thinks two fragments are related, the strongest signal wins.
   Hub suppression downweights over-connected nodes (e.g. common
   utilities) to prevent them from dominating the graph.

6. **Relevance Scoring (PPR)** — Run Personalized PageRank seeded
   from core (changed) fragments. The damping factor α=0.60
   controls propagation depth: 60% chance of following an edge,
   40% chance of teleporting back to changed code. Convergence
   produces a relevance score per fragment.

7. **Budget-Aware Selection** — A lazy greedy algorithm selects
   fragments maximizing density (marginal utility per token). Core
   fragments are selected first, then expansion candidates ordered
   by a max-heap. A τ-based stopping threshold (relative to
   baseline density median) prevents noise accumulation.

### Edge Taxonomy: Six Perspectives on Code Relationships

The system intentionally models relationships from multiple
independent perspectives. Each catches blind spots the others miss.

**Semantic Edges** — Language-aware code dependencies.
Import/export resolution, function calls, type references, symbol
usage. 11 language-specific builders (Python, JavaScript/TypeScript,
Go, Rust, Java/Kotlin/Scala, C/C++, C#/.NET, Ruby, PHP, Swift,
Shell). Weights reflect type-system reliability: Rust symbol refs
(0.95) are trusted more than Python calls (0.55) because static
analysis is more reliable in strict type systems. All semantic
edges are asymmetric — "A imports B" is a stronger signal than
"B is imported by A" — modeled via reverse weight factors
(0.4–0.7).

**Configuration Edges** — Infrastructure-to-code dependencies that
don't appear in source. Docker COPY/FROM to source files,
Kubernetes manifests to application code, Terraform modules to
infrastructure scripts, CI/CD workflows to tested code, Helm
templates to services, build system configs to compiled sources,
generic config keys to code referencing them. 7 specialized
builders covering the DevOps ecosystem.

**Structural Edges** — Filesystem and organizational proximity.
Containment (parent-child directory nesting), test-code
associations (naming heuristics like `test_foo.py` to `foo.py`),
sibling files in the same directory. These are weak signals
(0.05–0.60) that prevent blind spots in code without explicit
imports.

**Document Edges** — Non-code content relationships.
Section-to-section flow within Markdown, anchor link references,
cross-document citations. Enable following documentation
dependencies when docs change alongside code.

**Similarity Edges** — Content-based relationships via TF-IDF
lexical matching. Finds code with similar vocabulary/structure
even without explicit references. Weight bounds are
language-specific: wider for dynamic languages (Python 0.20–0.35),
narrower for typed (Rust 0.10–0.15) where semantic edges are more
reliable.

**History Edges** — Temporal co-change patterns from git log.
Files repeatedly committed together have implicit coupling.
Capped at 500 recent commits with noise filtering (ignoring large
commits with >30 files).

### Selection: Submodular Utility Maximization

The greedy selector optimizes a submodular utility function under
a token budget constraint:

**Concept coverage** — Each diff concept (identifier from the
change) has a "best coverage score" across selected fragments.
Adding a fragment that covers new concepts yields high marginal
gain; covering already-covered concepts yields diminishing returns
(modeled via square-root scaling).

**Relatedness bonus** — High-PPR fragments receive minimum
guaranteed utility even without concept overlap, ensuring
structurally related code is included.

**Density ordering** — Candidates are ranked by utility-per-token
(density), not raw utility. A 10-token fragment covering 2
concepts beats a 500-token fragment covering 3. Lazy heap
evaluation avoids recomputing stale density values until a
candidate is popped.

**τ-stopping** — After establishing a baseline from the first 5
selected fragments, stop when density drops below
τ × median(baseline). This relative threshold adapts to the
codebase: dense code triggers earlier stopping, sparse code allows
broader inclusion.

### Fragment Granularity

Files are decomposed into semantic fragments using a
priority-ordered parser pipeline. Language-specific parsers
(tree-sitter for 13+ languages, Python AST, Mistune for Markdown)
produce function/class/section-level fragments. Fallback parsers
handle config files (key-value boundaries), text (sentence-aware
splitting), and generic content (line-count limits). The
granularity choice means PPR reasons at the right level — a
changed line in a function selects that function as a unit, not
the whole file.

### Key Design Decisions

**Why Personalized PageRank over call-graph BFS?** BFS requires
arbitrary depth limits and treats all edges equally. PPR provides
natural exponential decay, respects edge weights, and converges
to a principled relevance distribution.

**Why max-aggregation for edge combination?** Multiple edge types
often agree on the same relationship. Taking the max avoids
inflating weights through redundant signals while preserving the
strongest evidence from any perspective.

**Why submodular greedy over knapsack?** Submodular functions
guarantee that greedy gives (1 - 1/e) ≈ 63% of optimal. With
lazy evaluation and density ordering, the algorithm runs in
near-linear time while achieving strong coverage.

**Why asymmetric edge weights?** Code dependencies are
directional. "A imports B" means A needs B for context; B doesn't
necessarily need A. Reverse factors (0.4–0.7 of forward weight)
enable bidirectional graph search while respecting this asymmetry.

**Why hub suppression?** Common utility modules (logging, helpers,
config) receive edges from everywhere. Without dampening, they
dominate PPR scores and pull in unrelated code. Log-scaled
in-degree suppression at the 95th percentile keeps them accessible
without letting them dominate.

### Tunable Parameters

| Parameter  | Default | Controls                          |
|------------|---------|-----------------------------------|
| `--budget` | 50000   | Maximum output tokens             |
| `--alpha`  | 0.60    | PPR damping — broader propagation |
| `--tau`    | 0.08    | Stopping — stricter = less noise  |
| `--full`   | false   | Bypass smart selection            |

---

## Technology Choices

| Decision    | Choice            | Rationale                    |
|-------------|-------------------|------------------------------|
| Output      | YAML              | LLM-readable, literal blocks |
| Tokens      | tiktoken o200k    | GPT-4o standard, exact BPE   |
| Ignores     | pathspec          | gitignore-compatible         |
| Parsing     | tree-sitter       | 13+ languages, AST-level     |
| Ranking     | PPR               | Relevance with natural decay |
| Selection   | Lazy greedy       | Near-optimal, linear time    |
| Git         | subprocess UTF-8  | Platform-safe, non-ASCII     |
| Diff        | git diff unified=0| Exact line ranges            |

## License

Apache 2.0
