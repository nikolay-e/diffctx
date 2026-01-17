# TreeMapper

> Extends [../CLAUDE.md](../CLAUDE.md)

[![PyPI](https://img.shields.io/pypi/v/treemapper)](https://pypi.org/project/treemapper/)
[![Downloads](https://img.shields.io/pypi/dm/treemapper)](https://pypi.org/project/treemapper/)
[![License](https://img.shields.io/github/license/nikolay-e/treemapper)](https://github.com/nikolay-e/treemapper/blob/main/LICENSE)

**Export your codebase for AI/LLM context in one command.**

```bash
pip install treemapper
treemapper . -o context.yaml   # paste into ChatGPT/Claude
```

## Why TreeMapper?

Unlike `tree` or `find`, TreeMapper exports **structure + file contents** in a
format optimized for LLM context windows:

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
treemapper .                          # YAML to stdout + token count
treemapper . -o tree.yaml             # save to file
treemapper . -o                       # save to tree.yaml (default filename)
treemapper . -o -                     # explicit stdout output
treemapper . -f json                  # JSON format
treemapper . -f txt                   # plain text with indentation
treemapper . -f md                    # Markdown with headings and fenced code blocks
treemapper . -f yml                   # YAML format (alias for yaml)
treemapper . --no-content             # structure only (no file contents)
treemapper . --max-depth 3            # limit directory depth
treemapper . --max-file-bytes 10000   # skip files larger than 10KB
treemapper . --max-file-bytes 0       # no limit (include all files)
treemapper . -i custom.ignore         # custom ignore patterns
treemapper . --no-default-ignores     # disable default ignores
treemapper . --log-level info         # log level (error/warning/info/debug)
treemapper . -c                       # copy to clipboard (no stdout)
treemapper . -c -o tree.yaml          # copy to clipboard + save to file
treemapper -v                         # show version

# Diff context mode
treemapper . --diff HEAD~1..HEAD      # context for recent changes
treemapper . --diff main..feature     # context for feature branch
treemapper . --diff HEAD~1 --budget 30000  # limit output tokens
treemapper . --diff HEAD~1 --full     # include all changed code
```

## Token Counting

Token count and size are always displayed on stderr:

```text
12,847 tokens (o200k_base), 52.3 KB
Copied to clipboard
```

For large outputs (>1MB), approximate counts are shown with `~` prefix:

```text
~125,000 tokens (o200k_base), 5.2 MB
```

Uses tiktoken with `o200k_base` encoding (GPT-4o tokenizer).

## Clipboard Support

Copy output directly to clipboard with `-c` or `--copy`:

```bash
treemapper . -c                       # copy to clipboard (no stdout)
treemapper . -c -o tree.yaml          # copy to clipboard + save to file
```

**System Requirements:**

- **macOS:** `pbcopy` (pre-installed)
- **Windows:** `clip` (pre-installed)
- **Linux/FreeBSD (Wayland):** `wl-copy` (install: `sudo apt install wl-clipboard`)
- **Linux/FreeBSD (X11):** `xclip` or `xsel` (install: `sudo apt install xclip`)

If clipboard is unavailable, output falls back to stdout with a warning on stderr.

## Python API

```python
from treemapper import map_directory, to_yaml, to_json, to_text, to_markdown

# Full function signature
tree = map_directory(
    path,                              # directory path (str or Path)
    max_depth=None,                    # limit traversal depth
    no_content=False,                  # exclude file contents
    max_file_bytes=None,               # skip files larger than N bytes
    ignore_file=None,                  # custom ignore file path
    no_default_ignores=False,          # disable .gitignore/.treemapperignore
)

# Examples
tree = map_directory("./myproject")
tree = map_directory("./src", max_depth=2, no_content=True)
tree = map_directory(".", max_file_bytes=50000, ignore_file="custom.ignore")

# Serialize to string
yaml_str = to_yaml(tree)
json_str = to_json(tree)
text_str = to_text(tree)    # or to_txt(tree)
md_str = to_markdown(tree)  # or to_md(tree)
```

## Ignore Patterns

Respects `.gitignore` and `.treemapperignore` automatically.
Use `--no-default-ignores` to include everything.

Features:

- Hierarchical: nested `.gitignore`/`.treemapperignore` files work at each
  directory level
- Negation patterns: `!important.log` un-ignores a file
- Anchored patterns: `/root_only.txt` matches only in root, `*.log` matches everywhere
- Output file is always auto-ignored (prevents recursive inclusion)

## Content Placeholders

When file content cannot be read normally, placeholders are used:

- `<file too large: N bytes>` — file exceeds `--max-file-bytes` limit
  (default: 10 MB)
- `<binary file: N bytes>` — binary file (detected by extension or null bytes)
- `<unreadable content: not utf-8>` — file is not valid UTF-8
- `<unreadable content>` — file cannot be read (permission denied, I/O error)

## Development

```bash
pip install -e ".[dev]"
pytest
pre-commit run --all-files
```

## Testing

Integration tests only - test against real filesystem. No mocking.

## Diff Context Mode

Smart context selection for git diffs using personalized PageRank:

```bash
treemapper . --diff HEAD~1..HEAD
```

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

Options:

- `--budget N` — token budget (default: 50000)
- `--alpha F` — PPR damping factor (default: 0.60)
- `--tau F` — stopping threshold (default: 0.08)
- `--full` — skip smart selection, include all changed code

## Architecture

```text
src/treemapper/
├── cli.py        # argument parsing
├── clipboard.py  # clipboard copy support
├── ignore.py     # gitignore/treemapperignore handling
├── tokens.py     # token counting (tiktoken)
├── tree.py       # directory traversal
├── writer.py     # YAML/JSON/text/Markdown output
├── treemapper.py # main entry point
└── diffctx/      # diff context mode
    ├── __init__.py       # entry point, run_diff_context()
    ├── fragments.py      # file fragmenters (Python, Markdown, etc.)
    ├── git.py            # git diff parsing
    ├── graph.py          # dependency graph building
    ├── ppr.py            # personalized PageRank
    ├── python_semantics.py  # Python import/call analysis
    ├── render.py         # output formatting
    ├── select.py         # greedy budget selection
    ├── stopwords.py      # identifier filtering
    ├── types.py          # Fragment, DiffHunk types
    └── utility.py        # submodular utility functions
```

## License

Apache 2.0
