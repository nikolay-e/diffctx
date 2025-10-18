# tests/test_output_formats.py
import json

from .conftest import run_treemapper_subprocess
from .utils import load_yaml


def test_yaml_format_output(temp_project):
    """Test YAML format output (default)."""
    output_file = temp_project / "output.yaml"

    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file), "--format", "yaml"])

    assert result.returncode == 0
    assert output_file.exists()

    # Verify it's valid YAML
    tree = load_yaml(output_file)
    assert tree["name"] == temp_project.name
    assert tree["type"] == "directory"
    assert "children" in tree


def test_json_format_output(temp_project):
    """Test JSON format output."""
    output_file = temp_project / "output.json"

    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file), "--format", "json"])

    assert result.returncode == 0
    assert output_file.exists()

    # Verify it's valid JSON
    with open(output_file, "r", encoding="utf-8") as f:
        tree = json.load(f)

    assert tree["name"] == temp_project.name
    assert tree["type"] == "directory"
    assert "children" in tree


def test_text_format_output(temp_project):
    """Test plain text format output."""
    output_file = temp_project / "output.txt"

    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file), "--format", "text"])

    assert result.returncode == 0
    assert output_file.exists()

    # Verify text format structure
    content = output_file.read_text(encoding="utf-8")
    assert f"Directory Tree: {temp_project.name}" in content
    assert "=" * 80 in content
    assert "--- BEGIN CONTENT ---" in content or content.count("=") >= 2


def test_json_format_with_content(temp_project):
    """Test JSON format includes file content."""
    # Create a test file
    test_file = temp_project / "test.txt"
    test_content = "Hello, JSON!"
    test_file.write_text(test_content, encoding="utf-8")

    output_file = temp_project / "output.json"

    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file), "--format", "json"])

    assert result.returncode == 0

    with open(output_file, "r", encoding="utf-8") as f:
        tree = json.load(f)

    # Find the test.txt file in the tree
    found = False
    for child in tree.get("children", []):
        if child.get("name") == "test.txt":
            assert "content" in child
            assert test_content in child["content"]
            found = True
            break

    assert found, "test.txt not found in JSON output"


def test_text_format_with_content(temp_project):
    """Test text format includes file content."""
    # Create a test file
    test_file = temp_project / "test.txt"
    test_content = "Hello, text format!"
    test_file.write_text(test_content, encoding="utf-8")

    output_file = temp_project / "output.txt"

    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file), "--format", "text"])

    assert result.returncode == 0

    content = output_file.read_text(encoding="utf-8")

    # Verify file is listed and content is included
    assert "test.txt" in content
    assert "--- BEGIN CONTENT ---" in content
    assert test_content in content
    assert "--- END CONTENT ---" in content


def test_json_format_stdout(temp_project):
    """Test JSON format output to stdout."""
    result = run_treemapper_subprocess([str(temp_project), "--format", "json"])

    assert result.returncode == 0

    # Parse JSON from stdout
    tree = json.loads(result.stdout)
    assert tree["name"] == temp_project.name
    assert tree["type"] == "directory"


def test_text_format_stdout(temp_project):
    """Test text format output to stdout."""
    result = run_treemapper_subprocess([str(temp_project), "--format", "text"])

    assert result.returncode == 0

    # Verify text format in stdout
    assert f"Directory Tree: {temp_project.name}" in result.stdout
    assert "=" * 80 in result.stdout


def test_yaml_multiline_content_preserves_newlines(temp_project):
    """Test that YAML correctly preserves multi-line content."""
    # Create a file with multi-line content
    test_file = temp_project / "multiline.txt"
    test_content = "Line 1\nLine 2\nLine 3\n"
    test_file.write_text(test_content, encoding="utf-8")

    output_file = temp_project / "output.yaml"

    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file), "--format", "yaml"])

    assert result.returncode == 0

    # Load YAML and verify content is preserved correctly
    tree = load_yaml(output_file)

    # Find the multiline.txt file
    for child in tree.get("children", []):
        if child.get("name") == "multiline.txt":
            content = child.get("content", "")
            # Content should preserve all lines and newlines
            assert "Line 1" in content
            assert "Line 2" in content
            assert "Line 3" in content
            # Should have the actual newline characters when parsed
            lines = content.split("\n")
            assert len([line for line in lines if line.strip()]) >= 3
            break


def test_json_multiline_content_escaping(temp_project):
    """Test that JSON properly escapes multi-line content."""
    # Create a file with multi-line content
    test_file = temp_project / "multiline.txt"
    test_content = "Line 1\nLine 2\nLine 3\n"
    test_file.write_text(test_content, encoding="utf-8")

    output_file = temp_project / "output.json"

    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file), "--format", "json"])

    assert result.returncode == 0

    # Verify JSON is valid and contains escaped newlines
    with open(output_file, "r", encoding="utf-8") as f:
        tree = json.load(f)

    # Find the multiline.txt file
    for child in tree.get("children", []):
        if child.get("name") == "multiline.txt":
            content = child.get("content", "")
            # In JSON, the content should be properly parsed with actual newlines
            assert "Line 1" in content
            assert "Line 2" in content
            assert "Line 3" in content
            break


def test_format_option_invalid(temp_project):
    """Test that invalid format option fails gracefully."""
    result = run_treemapper_subprocess([str(temp_project), "--format", "invalid"])

    # Should fail with non-zero exit code
    assert result.returncode != 0
    assert "invalid choice" in result.stderr.lower()


def test_default_format_is_yaml(temp_project):
    """Test that default format is YAML when not specified."""
    output_file = temp_project / "output.yaml"

    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file)])

    assert result.returncode == 0
    assert output_file.exists()

    # Should be valid YAML
    tree = load_yaml(output_file)
    assert tree["name"] == temp_project.name
