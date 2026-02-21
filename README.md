# TreeMapper

[![CI](https://github.com/nikolay-e/treemapper/actions/workflows/ci.yml/badge.svg)](https://github.com/nikolay-e/treemapper/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/nikolay-e/treemapper/branch/main/graph/badge.svg)](https://codecov.io/gh/nikolay-e/treemapper)
[![PyPI](https://img.shields.io/pypi/v/treemapper)](https://pypi.org/project/treemapper/)
[![Python versions](https://img.shields.io/pypi/pyversions/treemapper)](https://pypi.org/project/treemapper/)
[![Downloads](https://img.shields.io/pypi/dm/treemapper)](https://pypi.org/project/treemapper/)
[![License](https://img.shields.io/pypi/l/treemapper)](https://pypi.org/project/treemapper/)

**Export your codebase for AI/LLM context in one command.**

```bash
pip install treemapper                    # core (no native extensions)
pip install 'treemapper[tree-sitter]'     # + AST parsing for 10 languages
treemapper . -o context.yaml              # paste into ChatGPT/Claude
```

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

## Usage

```bash
treemapper                               # current dir, YAML to stdout
treemapper .                          # YAML to stdout + token count
treemapper . -o tree.yaml             # save to file
treemapper . -o                       # save to tree.yaml (default)
treemapper . -o -                     # explicit stdout output
treemapper . -f json                  # JSON format
treemapper . -f txt                   # plain text with indentation
treemapper . -f md                    # Markdown with fenced code
treemapper . -f yml                   # YAML (alias)
treemapper . --no-content             # structure only
treemapper . --max-depth 3            # limit depth (0=root, 1=children)
treemapper . --max-file-bytes 10000   # skip files > 10KB (default: 10 MB)
treemapper . --max-file-bytes 0       # no limit
treemapper . -i custom.ignore         # custom ignore patterns
treemapper . --no-default-ignores     # disable .gitignore + defaults
treemapper . --log-level info         # log level (default: error)
treemapper . -c                       # copy to clipboard
treemapper . -c -o tree.yaml          # clipboard + save to file
treemapper -v                         # show version
```

## Diff Context Mode

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
    path,                    # directory path
    max_depth=None,          # limit traversal depth
    no_content=False,        # exclude file contents
    max_file_bytes=None,     # skip large files
    ignore_file=None,        # custom ignore file
    no_default_ignores=False,# disable default ignores
)

yaml_str = to_yaml(tree)
json_str = to_json(tree)
text_str = to_text(tree)
md_str = to_markdown(tree)
```

## Ignore Patterns

Respects `.gitignore` and `.treemapperignore` automatically.
Use `--no-default-ignores` to disable all ignore processing
(`.gitignore`, `.treemapperignore`, and built-in defaults).

- Hierarchical: nested ignore files at each directory level
- Negation patterns: `!important.log` un-ignores a file
- Anchored patterns: `/root_only.txt` matches only in root
- Output file is always auto-ignored

## Content Placeholders

- `<file too large: N bytes>` — exceeds `--max-file-bytes`
- `<binary file: N bytes>` — binary file detected
- `<unreadable content: not utf-8>` — not valid UTF-8
- `<unreadable content>` — permission denied or I/O error

## License

Apache 2.0
