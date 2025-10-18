# TreeMapper

A tool for converting directory structures to structured formats (YAML, JSON, or text), designed for use with Large Language Models (LLMs).
TreeMapper maps your entire codebase into a structured file, making it easy to analyze code, document projects, and
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

Generate a structured representation of a directory:

```bash
# Map current directory to stdout (YAML format)
treemapper .

# Map specific directory to stdout
treemapper /path/to/dir

# Save to a file
treemapper . -o my-tree.yaml

# Use "-" to explicitly output to stdout
treemapper . -o -

# Output in JSON format
treemapper . --format json

# Output in plain text format
treemapper . --format text -o output.txt

# Limit directory traversal depth
treemapper . --max-depth 3

# Skip file contents (structure only)
treemapper . --no-content

# Limit file size for content reading
treemapper . --max-file-bytes 10000

# Custom ignore patterns
treemapper . -i ignore.txt

# Disable all default ignores
treemapper . --no-default-ignores

# Combine multiple options
treemapper . -o tree.json --format json --max-depth 5 --max-file-bytes 50000
```

### Options

```
treemapper [OPTIONS] [DIRECTORY]

Arguments:
  DIRECTORY                    Directory to analyze (default: current directory)

Options:
  -o, --output-file PATH      Output file (default: stdout)
                             Use "-" to force stdout output
  --format {yaml,json,text}   Output format (default: yaml)
  -i, --ignore-file PATH      Custom ignore patterns file
  --no-default-ignores        Disable all default ignores (.gitignore, .treemapperignore, etc.)
  --max-depth N               Maximum depth to traverse (default: unlimited)
  --no-content                Skip reading file contents (structure-only mode)
  --max-file-bytes N          Maximum file size to read in bytes (default: unlimited)
                             Larger files will show a placeholder
  -v, --verbosity [0-3]       Logging verbosity (default: 0)
                             0=ERROR, 1=WARNING, 2=INFO, 3=DEBUG
  --version                   Show version and exit
  -h, --help                  Show this help
```

### Ignore Patterns

By default, TreeMapper ignores:

- The output file itself (when using `-o`)
- All `.git` directories
- Python cache directories (`__pycache__`, `.pytest_cache`, `.mypy_cache`, etc.)
- Python build artifacts (`*.pyc`, `*.egg-info`, `dist/`, `build/`, etc.)
- Patterns from `.gitignore` files (in the scanned directory and subdirectories)
- Patterns from `.treemapperignore` file (in the scanned root directory)
- Symbolic links (always skipped)

Use `--no-default-ignores` to disable all default ignores and only use patterns from `-i/--ignore-file`.

### Example Output

**YAML format (default):**
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

**JSON format (`--format json`):**
```json
{
  "name": "my-project",
  "type": "directory",
  "children": [
    {
      "name": "src",
      "type": "directory",
      "children": [
        {
          "name": "main.py",
          "type": "file",
          "content": "def main():\n    print(\"Hello World\")\n"
        }
      ]
    },
    {
      "name": "README.md",
      "type": "file",
      "content": "# My Project\nDocumentation here...\n"
    }
  ]
}
```

**Text format (`--format text`):**
```
================================================================================
Directory Tree: my-project
================================================================================

src/ (directory)
  main.py (file)
    --- BEGIN CONTENT ---
    def main():
        print("Hello World")
    --- END CONTENT ---

README.md (file)
  --- BEGIN CONTENT ---
  # My Project
  Documentation here...
  --- END CONTENT ---
```

## License

Apache License 2.0 - see [LICENSE](LICENSE) for details.
