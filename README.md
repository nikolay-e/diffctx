# TreeMapper

A tool for converting directory structures to YAML format, designed for use with Large Language Models (LLMs).
TreeMapper maps your entire codebase into a structured YAML file, making it easy to analyze code, document projects, and
work with AI tools.

[![Build Status](https://img.shields.io/github/actions/workflow/status/nikolay-e/TreeMapper/ci.yml)](https://github.com/nikolay-e/TreeMapper/actions)  
[![PyPI](https://img.shields.io/pypi/v/treemapper)](https://pypi.org/project/treemapper)  
[![License](https://img.shields.io/github/license/nikolay-e/TreeMapper)](https://github.com/nikolay-e/TreeMapper/blob/main/LICENSE)

## Installation

Requires Python 3.9+:

```bash
pip install treemapper
```

## Usage

Generate a YAML tree of a directory:

```bash
# Map current directory
treemapper . 

# Map specific directory
treemapper /path/to/dir

# Custom output file
treemapper . -o my-tree.yaml

# Custom ignore patterns
treemapper . -i ignore.txt

# Disable all default ignores
treemapper . --no-default-ignores
```

### Options

```
treemapper [OPTIONS] [DIRECTORY]

Arguments:
  DIRECTORY                    Directory to analyze (default: current directory)

Options:
  -o, --output-file FILE      Output YAML file (default: directory_tree.yaml)
  -i, --ignore-file FILE      Custom ignore patterns file
  --no-default-ignores        Disable all default ignores
  -v, --verbosity [0-3]       Logging verbosity (default: 2)
                             0=ERROR, 1=WARNING, 2=INFO, 3=DEBUG
  -h, --help                  Show this help
```

### Ignore Patterns

By default, TreeMapper ignores:

- The output file itself
- All `.git` directories
- Patterns from `.gitignore` files
- Patterns from `.treemapperignore` file

Use `--no-default-ignores` to disable all default ignores and only use patterns from `-i/--ignore-file`.

### Example Output

```yaml
name: my-project
type: directory
children:
  - name: src
    type: directory
    children:
      - name: main.py
        type: file
        content: |
          def main():
              print("Hello World")
  - name: README.md
    type: file
    content: |
      # My Project
      Documentation here...
```

## License

Apache License 2.0 - see [LICENSE](LICENSE) for details.