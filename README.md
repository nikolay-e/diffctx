# TreeMapper

[![CI](https://github.com/nikolay-e/treemapper/actions/workflows/ci.yml/badge.svg)](https://github.com/nikolay-e/treemapper/actions/workflows/ci.yml)
[![Quality Gate Status](https://sonarcloud.io/api/project_badges/measure?project=nikolay-e_TreeMapper&metric=alert_status)](https://sonarcloud.io/summary/new_code?id=nikolay-e_TreeMapper)
[![codecov](https://codecov.io/gh/nikolay-e/treemapper/branch/main/graph/badge.svg)](https://codecov.io/gh/nikolay-e/treemapper)
[![PyPI](https://img.shields.io/pypi/v/treemapper)](https://pypi.org/project/treemapper/)
[![Python versions](https://img.shields.io/pypi/pyversions/treemapper)](https://pypi.org/project/treemapper/)
[![Downloads](https://img.shields.io/pypi/dm/treemapper)](https://pypi.org/project/treemapper/)
[![License](https://img.shields.io/pypi/l/treemapper)](https://pypi.org/project/treemapper/)

**Export your codebase for AI/LLM context in one command.**

```bash
pipx install treemapper          # install
treemapper . -o context.yaml    # paste into ChatGPT/Claude
```

![demo](docs/demo.gif)

## Why TreeMapper?

Unlike `tree` or `find`, TreeMapper exports **structure + file
contents** in a format optimized for fast comprehension:

```yaml
name: myproject
type: directory
children:
  - name: main.py
    type: file
    content: |
      def hello():
          print("Hello, World!")
  - name: utils/
    type: directory
    children:
      - name: helpers.py
        type: file
        content: |
          def add(a, b):
              return a + b
```

| Feature                  | `tree` | repomix | **TreeMapper** |
|--------------------------|:------:|:-------:|:--------------:|
| File contents            | ✗      | ✓       | ✓              |
| Token counting           | ✗      | ✓       | ✓              |
| Smart diff context       | ✗      | ✗       | ✓              |
| Multiple output formats  | ✗      | limited | YAML/JSON/MD/txt |
| Python API               | ✗      | ✗       | ✓              |
| 100% local / offline     | ✓      | ✓       | ✓              |

## Installation

```bash
pipx install treemapper                    # recommended: isolated, no venv needed
pip install treemapper                     # or with pip
pip install 'treemapper[tree-sitter]'      # + AST parsing for smarter diff context
```

**Standalone binary** (no Python required): download from the
[releases page](https://github.com/nikolay-e/treemapper/releases/latest).

> Diff context mode works out of the box. Adding `[tree-sitter]` enables AST-level
> parsing for more accurate context selection across 10 languages.

## Usage

<!-- BEGIN USAGE -->
```bash
treemapper                                  # current dir, YAML to stdout
treemapper .                                # YAML to stdout + token count
treemapper . -o tree.yaml                   # save to file
treemapper . --save                         # save to tree.yaml (default name)
treemapper . -o -                           # explicit stdout
treemapper . -f json                        # JSON format
treemapper . -f txt                         # plain text with indentation
treemapper . -f md                          # Markdown with fenced code blocks
treemapper . --no-content                   # structure only, no file contents
treemapper . --max-depth 3                  # limit depth (0=root only)
treemapper . --max-file-bytes 10000         # skip files > 10KB (default: 10 MB)
treemapper . --no-file-size-limit           # include all files regardless of size
treemapper . -i custom.ignore               # custom ignore patterns
treemapper . -w whitelist                   # include-only filter
treemapper . --no-default-ignores           # disable built-in ignore patterns
treemapper . --log-level info               # log level (default: error)
treemapper . -c                             # copy to clipboard
treemapper . -c -o tree.yaml                # clipboard + save to file
treemapper -v                               # show version

# diff context mode (requires git repo):
treemapper . --diff HEAD~1                  # context for last commit
treemapper . --diff main..feature           # context for feature branch
treemapper . --diff HEAD~1 --budget 30000   # limit diff context to ~30k tokens
treemapper . --diff HEAD~1 --full           # all changed code, no smart selection
treemapper . --diff HEAD~1 -c               # diff context to clipboard
```
<!-- END USAGE -->

## Diff Context Mode

**Paper:** [Context-Selection for Git Diff (Zenodo, 2026)](https://doi.org/10.5281/zenodo.18824580)

Smart context selection for git diffs — automatically finds the
minimal set of code fragments needed to understand a change:

```bash
treemapper . --diff HEAD~1..HEAD      # recent changes
treemapper . --diff main..feature     # feature branch
treemapper . --diff HEAD~1 --budget 30000  # limit tokens
treemapper . --diff HEAD~1 --full     # all changed code
```

Uses graph-based relevance propagation (Personalized PageRank)
to select the most important context. Output size is controlled
by algorithm convergence (τ-stopping) by default, or an explicit
`--budget` token limit. Understands imports, type references,
config dependencies, and co-change patterns across 15+
programming languages.

Output format:

```yaml
name: myproject
type: diff_context
fragment_count: 5
fragments:
  - path: src/main.py
    lines: "10-25"
    kind: function
    symbol: process_data
    content: |
      def process_data(items):
          ...
```

| Flag       | Default       | Description                                    |
|------------|---------------|------------------------------------------------|
| `--budget` | none          | Token limit (convergence-based by default)     |
| `--alpha`  | 0.60          | PPR damping factor                             |
| `--tau`    | 0.08          | Stopping threshold                             |
| `--full`   | false         | Include all changed code                       |

## Token Counting

Token count and size are always displayed on stderr:

```text
12,847 tokens (o200k_base), 52.3 KB
```

For large outputs (>1MB), approximate counts with `~` prefix:

```text
~125,000 tokens (o200k_base), 5.2 MB
```

Uses tiktoken with `o200k_base` encoding (GPT-4o tokenizer).

## Clipboard Support

Copy output directly to clipboard with `-c` or `--copy`:

```bash
treemapper . -c                       # copy (no stdout)
treemapper . -c -o tree.yaml          # copy + save to file
```

**System Requirements:**

- **macOS:** `pbcopy` (pre-installed)
- **Windows:** `clip` (pre-installed)
- **Linux (Wayland):** `wl-copy`
- **Linux (X11):** `xclip` or `xsel`

## Python API

```python
from treemapper import map_directory
from treemapper import to_yaml, to_json, to_text, to_markdown

tree = map_directory(
    path,                     # directory path
    max_depth=None,           # limit traversal depth
    no_content=False,         # exclude file contents
    max_file_bytes=None,      # skip large files
    ignore_file=None,         # custom ignore file
    no_default_ignores=False, # disable default ignores
    whitelist_file=None,      # include-only filter
)

yaml_str = to_yaml(tree)
json_str = to_json(tree)
text_str = to_text(tree)
md_str = to_markdown(tree)

# Diff context mode
from treemapper import build_diff_context

ctx = build_diff_context(
    root_dir,                 # Path to repository root
    diff_range,               # e.g. "HEAD~1..HEAD", "main..feature"
    budget_tokens=None,       # token limit (None = convergence-based)
    alpha=0.6,                # PPR damping factor
    tau=0.08,                 # stopping threshold
    full=False,               # skip smart selection
)
yaml_str = to_yaml(ctx)
```

## Ignore Patterns

Respects `.gitignore` and `.treemapper/ignore` automatically.
Use `--no-default-ignores` to disable built-in patterns
(`.gitignore` and `.treemapper/ignore` still apply).

- Hierarchical: nested ignore files at each directory level
- Negation patterns: `!important.log` un-ignores a file
- Anchored patterns: `/root_only.txt` matches only in root
- Output file is always auto-ignored

Auto-discovered files:

- `.treemapper/ignore` — TreeMapper-specific ignore patterns
- `.treemapper/whitelist` — Include-only filter (only matched files included)

## Content Placeholders

- `<file too large: N bytes>` — exceeds `--max-file-bytes`
- `<binary file: N bytes>` — binary file detected
- `<unreadable content: not utf-8>` — not valid UTF-8
- `<unreadable content>` — permission denied or I/O error

## License

Apache 2.0
