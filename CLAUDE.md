# CLAUDE.md - TreeMapper

## Project Overview

treemapper is a Python tool that converts directory structures to YAML format, designed specifically for use with Large Language Models (LLMs). It maps entire codebases into structured YAML files, making it easy to analyze code, document projects, and work with AI tools.

## Installation

Requires Python 3.9+:

```bash
pip install treemapper
```

## Development Environment

- Python 3.9+ required
- Package dependencies: pathspec, pyyaml

## Building and Installation

```bash
# Install in development mode
pip install -e .

# Install with development dependencies
pip install -e ".[dev]"

# Build distribution package
python -m build

# Install from PyPI
pip install treemapper
```

## Testing

```bash
# Run all tests
pytest

# Run specific test file
pytest tests/test_basic.py

# Run specific test
pytest tests/test_basic.py::test_basic_mapping

# Run tests with coverage
pytest --cov=src/treemapper

# Run tests in verbose mode
pytest -v
```

## Linting and Formatting

```bash
# Run flake8 linter
flake8 src/treemapper

# Run black formatter
black src/treemapper

# Run type checking with mypy
mypy src/treemapper

# Run autoflake to remove unused imports
autoflake --remove-all-unused-imports -i src/treemapper/*.py

# Run pre-commit hooks on all files
pre-commit run --all-files

# Run isort (import sorting)
isort src/treemapper tests
```

## Code Quality Tools

The project includes comprehensive code quality checks via CI and pre-commit hooks:

```bash
# Complexity analysis
radon cc src/treemapper/ --min B  # Cyclomatic complexity
radon mi src/treemapper/ --min B  # Maintainability index

# Mutation testing (test effectiveness)
mutmut run --paths-to-mutate=src/treemapper/

# Architecture checks (import contracts)
lint-imports

# Coverage reporting
pytest --cov=src/treemapper --cov-report=html
open htmlcov/index.html
```

### CI/CD Workflows

The project has two CI/CD workflows:

1. **Main CI** (`.github/workflows/ci.yml`): Comprehensive quality checks
   - Pre-commit hook validation (all hooks)
   - Linting and type checking (flake8, black, mypy)
   - Cross-platform testing (Linux, macOS, Windows)
   - Python version matrix (3.9, 3.10, 3.11, 3.12)
   - PyPy compatibility testing (pypy-3.9, pypy-3.10)
   - Test coverage with 80% threshold and branch analysis
   - Mutation testing (test effectiveness validation)
   - Complexity and maintainability metrics (radon)
   - Architecture/import contract validation (import-linter)
   - SonarCloud quality gate (code quality analysis)

2. **CD (Release)** (`.github/workflows/cd.yml`): Atomic releases
   - Version bump with git bundles
   - Multi-platform binary builds (Linux, macOS, Windows)
   - PyPI publishing (optional)
   - GitHub release creation with assets

## Project Architecture

The codebase is organized as follows:

- `src/treemapper/`: Main package
  - `treemapper.py`: Entry point and main orchestration
  - `cli.py`: Command-line argument parsing
  - `ignore.py`: Logic for handling ignore patterns (gitignore, treemapperignore)
  - `tree.py`: Core tree building functionality
  - `writer.py`: YAML output formatting and file writing
  - `logger.py`: Logging configuration

The application flow is:
1. Parse command-line arguments (`cli.py`)
2. Set up logging based on verbosity level (`logger.py`)
3. Load ignore patterns from various sources (`ignore.py`)
4. Build the directory tree structure (`tree.py`)
5. Write the tree structure to a YAML file (`writer.py`)

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

# Set verbosity level (0=ERROR, 1=WARNING, 2=INFO, 3=DEBUG)
treemapper . -v 3
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

By default, treemapper ignores:

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

## Creating a Distribution Package

```bash
# Build package
python -m build

# Create executable with PyInstaller
pyinstaller treemapper.spec
```

## License

Apache License 2.0 - see [LICENSE](LICENSE) for details.
