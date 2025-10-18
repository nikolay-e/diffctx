# tests/test_stdout_output.py
import sys

import pytest
import yaml

from .conftest import run_treemapper_subprocess
from .utils import load_yaml


def run_cli_command(args, cwd):
    """Run treemapper as a separate process."""
    return run_treemapper_subprocess(args, cwd=cwd)


def test_stdout_output_with_dash(temp_project):
    """Test output to stdout using -o - option."""
    # Create a simple structure
    (temp_project / "test.txt").write_text("test content", encoding="utf-8")
    (temp_project / "subdir").mkdir()
    (temp_project / "subdir" / "file.py").write_text("print('hello')", encoding="utf-8")

    # Run with -o - to output to stdout
    result = run_cli_command([".", "-o", "-"], cwd=temp_project)

    assert result.returncode == 0
    assert result.stdout.strip() != ""

    # Parse the YAML from stdout
    try:
        tree_data = yaml.safe_load(result.stdout)
    except yaml.YAMLError as e:
        pytest.fail(f"Failed to parse YAML from stdout: {e}")

    # Verify the structure
    assert tree_data["type"] == "directory"
    assert tree_data["name"] == temp_project.name

    # Check that files are included
    children_names = [child["name"] for child in tree_data.get("children", [])]
    assert "test.txt" in children_names
    assert "subdir" in children_names

    # Verify no file was created
    assert not (temp_project / "directory_tree.yaml").exists()


def test_stdout_output_with_double_dash(temp_project):
    """Test output to stdout using --output-file - option."""
    # Create a test file
    (temp_project / "example.txt").write_text("example content", encoding="utf-8")

    # Run with --output-file - to output to stdout
    result = run_cli_command([".", "--output-file", "-"], cwd=temp_project)

    assert result.returncode == 0
    assert result.stdout.strip() != ""

    # Parse and verify
    tree_data = yaml.safe_load(result.stdout)
    assert tree_data["type"] == "directory"

    # Verify no file was created
    assert not (temp_project / "directory_tree.yaml").exists()


def test_stdout_output_preserves_stderr_logging(temp_project):
    """Test that logging still goes to stderr when outputting to stdout."""
    # Run with verbose logging and stdout output
    result = run_cli_command([".", "-o", "-", "-v", "2"], cwd=temp_project)

    assert result.returncode == 0
    assert result.stdout.strip() != ""  # YAML output in stdout
    assert "INFO" in result.stderr  # Logging in stderr
    assert "Directory tree written to stdout" in result.stderr

    # Verify stdout contains valid YAML
    tree_data = yaml.safe_load(result.stdout)
    assert tree_data["type"] == "directory"


def test_stdout_output_with_file_content(temp_project):
    """Test that file contents are included in stdout output."""
    # Create a file with content
    test_content = "def hello():\n    print('Hello, World!')\n"
    (temp_project / "hello.py").write_text(test_content, encoding="utf-8")

    # Run with stdout output
    result = run_cli_command([".", "-o", "-"], cwd=temp_project)

    assert result.returncode == 0

    # Parse the output
    tree_data = yaml.safe_load(result.stdout)

    # Find the hello.py file in the tree
    hello_file = None
    for child in tree_data.get("children", []):
        if child.get("name") == "hello.py":
            hello_file = child
            break

    assert hello_file is not None
    assert hello_file["type"] == "file"
    assert hello_file["content"] == test_content


def test_stdout_output_respects_ignore_patterns(temp_project):
    """Test that ignore patterns work with stdout output."""
    # Create files
    (temp_project / "include.txt").write_text("included", encoding="utf-8")
    (temp_project / "exclude.txt").write_text("excluded", encoding="utf-8")

    # Create custom ignore file
    ignore_file = temp_project / "custom.ignore"
    ignore_file.write_text("exclude.txt\n")

    # Run with stdout output and custom ignore
    result = run_cli_command([".", "-o", "-", "-i", str(ignore_file)], cwd=temp_project)

    assert result.returncode == 0

    # Parse the output
    tree_data = yaml.safe_load(result.stdout)

    # Check that excluded file is not in output
    children_names = [child["name"] for child in tree_data.get("children", [])]
    assert "include.txt" in children_names
    assert "exclude.txt" not in children_names


def test_stdout_output_with_empty_directory(temp_project):
    """Test stdout output with an empty directory."""
    # Create an empty subdirectory
    empty_dir = temp_project / "empty_dir"
    empty_dir.mkdir()

    # Run with stdout output
    result = run_cli_command([str(empty_dir), "-o", "-"], cwd=temp_project)

    assert result.returncode == 0

    # Parse the output
    tree_data = yaml.safe_load(result.stdout)

    assert tree_data["type"] == "directory"
    assert tree_data["name"] == "empty_dir"
    assert tree_data.get("children", []) == []


def test_stdout_output_with_special_characters(temp_project):
    """Test stdout output with files containing special characters."""
    # Create files with special characters in names and content
    special_content = "Special chars: é ñ ü 中文 🚀\n"
    (temp_project / "special_chars.txt").write_text(special_content, encoding="utf-8")

    # Run with stdout output
    result = run_cli_command([".", "-o", "-"], cwd=temp_project)

    assert result.returncode == 0

    # Parse the output
    tree_data = yaml.safe_load(result.stdout)

    # Find the special file
    special_file = None
    for child in tree_data.get("children", []):
        if child.get("name") == "special_chars.txt":
            special_file = child
            break

    assert special_file is not None
    assert special_file["content"] == special_content


def test_stdout_output_large_tree(temp_project):
    """Test stdout output with a larger directory tree."""
    # Create a more complex structure
    for i in range(3):
        subdir = temp_project / f"dir_{i}"
        subdir.mkdir()
        for j in range(3):
            (subdir / f"file_{j}.txt").write_text(f"Content {i}-{j}", encoding="utf-8")
            nested = subdir / f"nested_{j}"
            nested.mkdir()
            (nested / "deep.txt").write_text("Deep content", encoding="utf-8")

    # Run with stdout output
    result = run_cli_command([".", "-o", "-"], cwd=temp_project)

    assert result.returncode == 0

    # Parse the output
    tree_data = yaml.safe_load(result.stdout)

    # Verify it's a valid tree structure
    assert tree_data["type"] == "directory"
    assert len(tree_data.get("children", [])) >= 3  # At least our 3 directories

    # Count total nodes to ensure everything was captured
    def count_nodes(node):
        count = 1
        for child in node.get("children", []):
            count += count_nodes(child)
        return count

    total_nodes = count_nodes(tree_data)
    assert total_nodes > 30  # We created at least this many files/dirs


def test_stdout_vs_file_output_consistency(temp_project):
    """Test that stdout output is identical to file output."""
    # Create some content
    (temp_project / "test.py").write_text("import os\n", encoding="utf-8")
    (temp_project / "data").mkdir()
    (temp_project / "data" / "info.txt").write_text("data", encoding="utf-8")

    # Get stdout output
    result_stdout = run_cli_command([".", "-o", "-"], cwd=temp_project)
    assert result_stdout.returncode == 0
    stdout_data = yaml.safe_load(result_stdout.stdout)

    # Get file output
    output_file = temp_project / "test_output.yaml"
    result_file = run_cli_command([".", "-o", str(output_file)], cwd=temp_project)
    assert result_file.returncode == 0
    assert output_file.exists()
    file_data = load_yaml(output_file)

    # Compare the structures (they should be identical)
    assert stdout_data == file_data


def test_stdout_output_error_handling(temp_project):
    """Test error handling with stdout output."""
    # Try to map a non-existent directory
    result = run_cli_command(["non_existent_dir", "-o", "-"], cwd=temp_project)

    assert result.returncode != 0
    assert "Error:" in result.stderr
    assert "does not exist" in result.stderr or "not a valid directory" in result.stderr
    # stdout should be empty on error
    assert result.stdout.strip() == ""


def test_stdout_output_with_permission_errors(temp_project, set_perms):
    """Test stdout output when some files are unreadable."""
    # This test is skipped on Windows
    if sys.platform == "win32":
        pytest.skip("Permission tests skipped on Windows.")

    # Create a readable and unreadable file
    (temp_project / "readable.txt").write_text("can read", encoding="utf-8")
    unreadable = temp_project / "unreadable.txt"
    unreadable.write_text("cannot read", encoding="utf-8")
    set_perms(unreadable, 0o000)

    # Run with stdout output and verbose logging
    result = run_cli_command([".", "-o", "-", "-v", "3"], cwd=temp_project)

    assert result.returncode == 0

    # Parse the output
    tree_data = yaml.safe_load(result.stdout)

    # Find both files
    files = {child["name"]: child for child in tree_data.get("children", [])}

    assert "readable.txt" in files
    assert "unreadable.txt" in files

    # Check content handling
    assert files["readable.txt"]["content"] == "can read\n"
    assert files["unreadable.txt"]["content"] == "<unreadable content>\n"

    # Check that error was logged to stderr
    assert "Could not read" in result.stderr
    assert "unreadable.txt" in result.stderr
