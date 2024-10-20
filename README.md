# TreeMapper

[![Build Status](https://img.shields.io/github/actions/workflow/status/nikolay-e/TreeMapper/ci.yml)](https://github.com/nikolay-e/TreeMapper/actions)  
[![PyPI](https://img.shields.io/pypi/v/treemapper)](https://pypi.org/project/treemapper)  
[![License](https://img.shields.io/github/license/nikolay-e/TreeMapper)](https://github.com/nikolay-e/TreeMapper/blob/main/LICENSE)

TreeMapper is a Python tool designed to convert directory structures and file contents into a YAML format, primarily for use with Large Language Models (LLMs). It helps in codebase analysis, project documentation, and interacting with AI tools by providing a readable representation of your project’s structure.

## Key Features

- Converts directory structures into a **YAML format**
- Captures **file contents** and includes them in the output
- Supports `.gitignore` and custom ignore files (`.treemapperignore`)
- Easy-to-use **CLI tool** with flexible options for input and output paths
- Works cross-platform (Linux, macOS, and Windows)

## Installation

TreeMapper requires **Python 3.9 or higher**.

You can install TreeMapper using pip:

```bash
pip install treemapper
```

## Quick Start

To quickly generate a YAML representation of the current directory and save it to `output.yaml`, run:

```bash
treemapper . -o output.yaml
```

## Usage

TreeMapper can be run from the command line with the following options:

```bash
treemapper [-i IGNORE_FILE] [-o OUTPUT_FILE] [--no-git-ignore] [-v VERBOSITY] [directory_path]
```

| Option                | Description                                                      |
|-----------------------|------------------------------------------------------------------|
| `directory_path`      | The directory to analyze (default: current directory)            |
| `-i, --ignore-file`   | Path to a custom ignore file                                     |
| `-o, --output-file`   | Path for the output YAML file (default: `./directory_tree.yaml`) |
| `--no-git-ignore`     | Disable git-related default ignores                              |
| `-v, --verbosity`     | Set verbosity level (0: ERROR, 1: WARNING, 2: INFO, 3: DEBUG)    |

### Example Commands

1. Analyze the current directory, respecting `.gitignore` and `.treemapperignore`:
```bash
python -m treemapper .
```

2. Analyze a specific directory with a custom ignore file:
```bash
python -m treemapper ./my_project -i custom_ignore.txt
```

3. Output the YAML representation to a different file:
```bash
python -m treemapper ./my_project -o project_structure.yaml
```

## Example Output

```yaml
name: example_directory
type: directory
children:
  - name: file1.txt
    type: file
    content: |
      This is the content of file1.txt
  - name: subdirectory
    type: directory
    children:
      - name: file2.py
        type: file
        content: |
          def hello_world():
              print("Hello, World!")
```

## Configuration

You can use a `.treemapperignore` file in your project directory to exclude specific files or directories from the YAML output. The format is similar to `.gitignore`.

## License

This project is licensed under the Apache License 2.0. See the [LICENSE](LICENSE) file for details.

---

