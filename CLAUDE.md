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

Unlike `tree` or `find`, TreeMapper exports **structure + file contents** in a format optimized for LLM context windows:

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
treemapper .                          # YAML to stdout
treemapper . -o tree.yaml             # save to file
treemapper . --format json            # JSON format
treemapper . --format text            # tree-style text
treemapper . --no-content             # structure only (no file contents)
treemapper . --max-depth 3            # limit directory depth
treemapper . --max-file-bytes 10000   # skip files larger than 10KB
treemapper . -i custom.ignore         # custom ignore patterns
```

## Python API

```python
from treemapper import map_directory, to_yaml, to_json, to_text

# Get tree as dict
tree = map_directory("./myproject")
tree = map_directory("./src", max_depth=2, no_content=True)

# Serialize to string
yaml_str = to_yaml(tree)
json_str = to_json(tree)
text_str = to_text(tree)
```

## Ignore Patterns

Respects `.gitignore` and `.treemapperignore` automatically. Use `--no-default-ignores` to include everything.

## Development

```bash
pip install -e ".[dev]"
pytest
pre-commit run --all-files
```

## Testing

Integration tests only - test against real filesystem. No mocking.

## Architecture

```
src/treemapper/
├── cli.py        # argument parsing
├── ignore.py     # gitignore/treemapperignore handling
├── tree.py       # directory traversal
├── writer.py     # YAML/JSON/text output
└── treemapper.py # main entry point
```

## License

Apache 2.0
