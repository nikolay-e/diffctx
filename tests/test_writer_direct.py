# tests/test_writer_direct.py
"""Direct unit tests for writer module to increase coverage."""
import io
import json

import pytest
import yaml

from treemapper.writer import (
    write_tree_json,
    write_tree_text,
    write_tree_to_file,
    write_tree_yaml,
)


def test_write_tree_json_direct():
    """Test write_tree_json function directly."""
    tree = {
        "name": "test",
        "type": "directory",
        "children": [{"name": "file.txt", "type": "file", "content": "hello\n"}],
    }

    output = io.StringIO()
    write_tree_json(output, tree)

    result = output.getvalue()
    parsed = json.loads(result)

    assert parsed["name"] == "test"
    assert parsed["type"] == "directory"
    assert len(parsed["children"]) == 1
    assert parsed["children"][0]["name"] == "file.txt"


def test_write_tree_yaml_direct():
    """Test write_tree_yaml function directly."""
    tree = {
        "name": "test",
        "type": "directory",
        "children": [{"name": "file.txt", "type": "file", "content": "hello\n"}],
    }

    output = io.StringIO()
    write_tree_yaml(output, tree)

    result = output.getvalue()
    parsed = yaml.safe_load(result)

    assert parsed["name"] == "test"
    assert parsed["type"] == "directory"
    assert len(parsed["children"]) == 1


def test_write_tree_text_direct():
    """Test write_tree_text function directly."""
    tree = {
        "name": "test_project",
        "type": "directory",
        "children": [
            {
                "name": "file.txt",
                "type": "file",
                "content": "line1\nline2\n",
            },
            {
                "name": "subdir",
                "type": "directory",
                "children": [{"name": "nested.txt", "type": "file", "content": "nested\n"}],
            },
        ],
    }

    output = io.StringIO()
    write_tree_text(output, tree)

    result = output.getvalue()

    assert "Directory Tree: test_project" in result
    assert "=" * 80 in result
    assert "file.txt" in result
    assert "subdir/" in result
    assert "--- BEGIN CONTENT ---" in result
    assert "line1" in result
    assert "line2" in result
    assert "--- END CONTENT ---" in result
    assert "nested.txt" in result
    assert "nested" in result


def test_write_tree_text_with_empty_content():
    """Test text format with empty file content."""
    tree = {
        "name": "test",
        "type": "directory",
        "children": [{"name": "empty.txt", "type": "file", "content": ""}],
    }

    output = io.StringIO()
    write_tree_text(output, tree)

    result = output.getvalue()
    assert "empty.txt" in result
    assert "--- BEGIN CONTENT ---" in result
    assert "--- END CONTENT ---" in result


def test_write_tree_text_no_content():
    """Test text format with file without content key."""
    tree = {
        "name": "test",
        "type": "directory",
        "children": [{"name": "file.txt", "type": "file"}],  # No content key
    }

    output = io.StringIO()
    write_tree_text(output, tree)

    result = output.getvalue()
    assert "file.txt" in result
    assert "--- BEGIN CONTENT ---" not in result


def test_write_tree_text_directory_marker():
    """Test that directories get '/' suffix in text format."""
    tree = {
        "name": "test",
        "type": "directory",
        "children": [{"name": "subdir", "type": "directory", "children": []}],
    }

    output = io.StringIO()
    write_tree_text(output, tree)

    result = output.getvalue()
    assert "subdir/" in result


def test_write_tree_text_nested_indentation():
    """Test indentation in text format for nested directories."""
    tree = {
        "name": "root",
        "type": "directory",
        "children": [
            {
                "name": "level1",
                "type": "directory",
                "children": [
                    {
                        "name": "level2",
                        "type": "directory",
                        "children": [{"name": "deep.txt", "type": "file"}],
                    }
                ],
            }
        ],
    }

    output = io.StringIO()
    write_tree_text(output, tree)

    result = output.getvalue()
    assert "level1/" in result
    assert "level2/" in result
    assert "deep.txt" in result


def test_write_tree_to_file_creates_parent_dirs(tmp_path):
    """Test that write_tree_to_file creates parent directories."""
    tree = {"name": "test", "type": "directory", "children": []}

    output_file = tmp_path / "nested" / "dir" / "output.yaml"

    write_tree_to_file(tree, output_file, "yaml")

    assert output_file.exists()
    assert output_file.parent.exists()


def test_write_tree_to_file_json_format(tmp_path):
    """Test write_tree_to_file with JSON format."""
    tree = {
        "name": "test",
        "type": "directory",
        "children": [{"name": "file.txt", "type": "file", "content": "test\n"}],
    }

    output_file = tmp_path / "output.json"

    write_tree_to_file(tree, output_file, "json")

    assert output_file.exists()

    with open(output_file, "r", encoding="utf-8") as f:
        parsed = json.load(f)

    assert parsed["name"] == "test"


def test_write_tree_to_file_text_format(tmp_path):
    """Test write_tree_to_file with text format."""
    tree = {
        "name": "test",
        "type": "directory",
        "children": [{"name": "file.txt", "type": "file", "content": "test\n"}],
    }

    output_file = tmp_path / "output.txt"

    write_tree_to_file(tree, output_file, "text")

    assert output_file.exists()

    content = output_file.read_text(encoding="utf-8")
    assert "Directory Tree: test" in content
    assert "file.txt" in content


def test_write_tree_to_file_directory_error(tmp_path):
    """Test error when output file is a directory."""
    tree = {"name": "test", "type": "directory", "children": []}

    # Create a directory instead of a file
    output_dir = tmp_path / "output_dir"
    output_dir.mkdir()

    with pytest.raises(IOError, match="Is a directory"):
        write_tree_to_file(tree, output_dir, "yaml")


def test_write_tree_yaml_multiline_content():
    """Test YAML output with multi-line content."""
    tree = {
        "name": "test",
        "type": "directory",
        "children": [{"name": "file.txt", "type": "file", "content": "line1\nline2\nline3\n"}],
    }

    output = io.StringIO()
    write_tree_yaml(output, tree)

    result = output.getvalue()
    parsed = yaml.safe_load(result)

    # Content should be preserved
    assert parsed["children"][0]["content"] == "line1\nline2\nline3\n"


def test_write_tree_json_unicode():
    """Test JSON output with Unicode characters."""
    tree = {
        "name": "test",
        "type": "directory",
        "children": [{"name": "файл.txt", "type": "file", "content": "Привет мир\n"}],
    }

    output = io.StringIO()
    write_tree_json(output, tree)

    result = output.getvalue()
    parsed = json.loads(result)

    assert parsed["children"][0]["name"] == "файл.txt"
    assert parsed["children"][0]["content"] == "Привет мир\n"


def test_write_tree_text_multiline_splits_correctly():
    """Test that text format splits multi-line content correctly."""
    tree = {
        "name": "test",
        "type": "directory",
        "children": [{"name": "file.txt", "type": "file", "content": "line1\nline2\nline3"}],
    }

    output = io.StringIO()
    write_tree_text(output, tree)

    result = output.getvalue()

    # Each line should be on its own line with indentation
    assert "line1" in result
    assert "line2" in result
    assert "line3" in result


def test_write_tree_to_file_stdout_yaml():
    """Test write_tree_to_file with None output_file (stdout) for YAML."""
    tree = {"name": "test", "type": "directory", "children": []}

    # Use StringIO to capture output instead of capsys to avoid UTF-8 wrapper issues
    import sys
    from io import StringIO

    old_stdout = sys.stdout
    sys.stdout = StringIO()

    try:
        write_tree_to_file(tree, None, "yaml")
        output = sys.stdout.getvalue()
    finally:
        sys.stdout = old_stdout

    parsed = yaml.safe_load(output)
    assert parsed["name"] == "test"


def test_write_tree_to_file_stdout_json():
    """Test write_tree_to_file with JSON to stdout."""
    tree = {"name": "test", "type": "directory", "children": []}

    import sys
    from io import StringIO

    old_stdout = sys.stdout
    sys.stdout = StringIO()

    try:
        write_tree_to_file(tree, None, "json")
        output = sys.stdout.getvalue()
    finally:
        sys.stdout = old_stdout

    parsed = json.loads(output)
    assert parsed["name"] == "test"


def test_write_tree_to_file_stdout_text():
    """Test write_tree_to_file with text to stdout."""
    tree = {"name": "test", "type": "directory", "children": []}

    import sys
    from io import StringIO

    old_stdout = sys.stdout
    sys.stdout = StringIO()

    try:
        write_tree_to_file(tree, None, "text")
        output = sys.stdout.getvalue()
    finally:
        sys.stdout = old_stdout

    assert "Directory Tree: test" in output
    assert "=" * 80 in output
