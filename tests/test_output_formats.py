# tests/test_output_formats.py
import io
import json

import pytest
import yaml

from treemapper import map_directory, to_json, to_text, to_yaml
from treemapper.writer import write_tree_json, write_tree_text, write_tree_to_file, write_tree_yaml

from .conftest import run_treemapper_subprocess
from .utils import load_yaml


@pytest.mark.parametrize(
    "fmt,ext",
    [
        ("yaml", ".yaml"),
        ("json", ".json"),
        ("txt", ".txt"),
    ],
)
def test_format_output_to_file(temp_project, fmt, ext):
    output_file = temp_project / f"output{ext}"
    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file), "--format", fmt])
    assert result.returncode == 0
    assert output_file.exists()

    content = output_file.read_text(encoding="utf-8")
    if fmt == "yaml":
        tree = yaml.safe_load(content)
        assert tree["name"] == temp_project.name
        assert tree["type"] == "directory"
    elif fmt == "json":
        tree = json.loads(content)
        assert tree["name"] == temp_project.name
        assert tree["type"] == "directory"
    else:
        assert f"{temp_project.name}/" in content
        assert "  " in content


@pytest.mark.parametrize("fmt", ["yaml", "json", "txt"])
def test_format_output_to_stdout(temp_project, fmt):
    result = run_treemapper_subprocess([str(temp_project), "--format", fmt])
    assert result.returncode == 0

    if fmt == "yaml":
        tree = yaml.safe_load(result.stdout)
        assert tree["name"] == temp_project.name
    elif fmt == "json":
        tree = json.loads(result.stdout)
        assert tree["name"] == temp_project.name
    else:
        assert f"{temp_project.name}/" in result.stdout


def test_python_api_serializers(temp_project):
    api_tree = map_directory(temp_project)

    yaml_str = to_yaml(api_tree)
    parsed_yaml = yaml.safe_load(yaml_str)
    assert parsed_yaml["name"] == temp_project.name

    json_str = to_json(api_tree)
    parsed_json = json.loads(json_str)
    assert parsed_json["name"] == temp_project.name

    text_str = to_text(api_tree)
    assert f"{temp_project.name}/" in text_str


def test_format_with_file_content(temp_project):
    test_file = temp_project / "test.txt"
    test_content = "Hello, format test!"
    test_file.write_text(test_content, encoding="utf-8")

    for fmt in ["yaml", "json", "txt"]:
        output_file = temp_project / f"output.{fmt}"
        result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file), "--format", fmt])
        assert result.returncode == 0

        content = output_file.read_text(encoding="utf-8")
        assert test_content in content


def test_multiline_content_preservation(temp_project):
    test_file = temp_project / "multiline.txt"
    test_content = "Line 1\nLine 2\nLine 3\n"
    test_file.write_text(test_content, encoding="utf-8")

    output_file = temp_project / "output.yaml"
    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file), "--format", "yaml"])
    assert result.returncode == 0

    tree = load_yaml(output_file)
    for child in tree.get("children", []):
        if child.get("name") == "multiline.txt":
            content = child.get("content", "")
            assert "Line 1" in content
            assert "Line 2" in content
            assert "Line 3" in content
            break


def test_format_option_invalid(temp_project):
    result = run_treemapper_subprocess([str(temp_project), "--format", "invalid"])
    assert result.returncode != 0
    assert "invalid choice" in result.stderr.lower()


def test_default_format_is_yaml(temp_project):
    output_file = temp_project / "output.yaml"
    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file)])
    assert result.returncode == 0
    tree = load_yaml(output_file)
    assert tree["name"] == temp_project.name


# --- Direct writer function tests ---


@pytest.mark.parametrize(
    "writer_func,parser",
    [
        (write_tree_yaml, yaml.safe_load),
        (write_tree_json, json.loads),
    ],
)
def test_writer_direct(writer_func, parser):
    tree = {
        "name": "test",
        "type": "directory",
        "children": [{"name": "file.txt", "type": "file", "content": "hello\n"}],
    }
    output = io.StringIO()
    writer_func(output, tree)
    result = output.getvalue()
    parsed = parser(result)
    assert parsed["name"] == "test"
    assert parsed["type"] == "directory"
    assert len(parsed["children"]) == 1


def test_write_tree_text_direct():
    tree = {
        "name": "test_project",
        "type": "directory",
        "children": [
            {"name": "file.txt", "type": "file", "content": "line1\nline2\n"},
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

    assert "test_project/" in result
    assert "├── file.txt" in result
    assert "└── subdir/" in result
    assert "nested.txt" in result


def test_write_tree_text_edge_cases():
    tree_empty = {"name": "test", "type": "directory", "children": [{"name": "empty.txt", "type": "file", "content": ""}]}
    output = io.StringIO()
    write_tree_text(output, tree_empty)
    result = output.getvalue()
    non_blank_lines = [line for line in result.strip().split("\n") if line.strip()]
    assert non_blank_lines[0] == "test/"
    assert any("empty.txt" in line for line in non_blank_lines)
    assert any("(empty file)" in line for line in non_blank_lines)

    tree_no_content = {"name": "test", "type": "directory", "children": [{"name": "file.txt", "type": "file"}]}
    output = io.StringIO()
    write_tree_text(output, tree_no_content)
    result = output.getvalue()
    non_blank_lines = [line for line in result.strip().split("\n") if line.strip()]
    assert non_blank_lines[0] == "test/"
    assert "file.txt" in non_blank_lines[-1]


def test_write_tree_to_file_creates_parent_dirs(tmp_path):
    tree = {"name": "test", "type": "directory", "children": []}
    output_file = tmp_path / "nested" / "dir" / "output.yaml"
    write_tree_to_file(tree, output_file, "yaml")
    assert output_file.exists()
    assert output_file.parent.exists()


@pytest.mark.parametrize("fmt", ["yaml", "json", "txt"])
def test_write_tree_to_file_formats(tmp_path, fmt):
    tree = {"name": "test", "type": "directory", "children": [{"name": "file.txt", "type": "file", "content": "test\n"}]}
    output_file = tmp_path / f"output.{fmt}"
    write_tree_to_file(tree, output_file, fmt)
    assert output_file.exists()
    content = output_file.read_text(encoding="utf-8")
    assert "test" in content


def test_write_tree_to_file_directory_error(tmp_path):
    tree = {"name": "test", "type": "directory", "children": []}
    output_dir = tmp_path / "output_dir"
    output_dir.mkdir()
    with pytest.raises(IOError, match="Is a directory"):
        write_tree_to_file(tree, output_dir, "yaml")


def test_write_tree_yaml_multiline_content():
    tree = {
        "name": "test",
        "type": "directory",
        "children": [{"name": "file.txt", "type": "file", "content": "line1\nline2\nline3\n"}],
    }
    output = io.StringIO()
    write_tree_yaml(output, tree)
    parsed = yaml.safe_load(output.getvalue())
    assert parsed["children"][0]["content"] == "line1\nline2\nline3\n"


def test_write_tree_json_unicode():
    tree = {
        "name": "test",
        "type": "directory",
        "children": [{"name": "файл.txt", "type": "file", "content": "Привет мир\n"}],
    }
    output = io.StringIO()
    write_tree_json(output, tree)
    parsed = json.loads(output.getvalue())
    assert parsed["children"][0]["name"] == "файл.txt"
    assert parsed["children"][0]["content"] == "Привет мир\n"


@pytest.mark.parametrize("fmt", ["yaml", "json", "txt"])
def test_write_tree_to_file_stdout(fmt):
    import sys
    from io import StringIO

    tree = {"name": "test", "type": "directory", "children": []}
    old_stdout = sys.stdout
    sys.stdout = StringIO()
    try:
        write_tree_to_file(tree, None, fmt)
        output = sys.stdout.getvalue()
    finally:
        sys.stdout = old_stdout

    if fmt == "yaml":
        parsed = yaml.safe_load(output)
        assert parsed["name"] == "test"
    elif fmt == "json":
        parsed = json.loads(output)
        assert parsed["name"] == "test"
    else:
        assert "test/" in output


class TestYamlEmitterEdgeCases:
    def _roundtrip(self, tree):
        output = io.StringIO()
        write_tree_yaml(output, tree)
        raw = output.getvalue()
        parsed = yaml.safe_load(raw)
        return parsed, raw

    def test_empty_string_content(self):
        tree = {"name": "p", "type": "directory", "children": [{"name": "e.txt", "type": "file", "content": ""}]}
        parsed, _ = self._roundtrip(tree)
        assert parsed["children"][0]["content"] == ""

    def test_whitespace_only_content(self):
        tree = {"name": "p", "type": "directory", "children": [{"name": "w.txt", "type": "file", "content": "   \n  \n"}]}
        parsed, _ = self._roundtrip(tree)
        assert parsed["children"][0]["content"] == "   \n  \n"

    @pytest.mark.parametrize(
        "filename",
        ["true", "false", "null", "yes", "no", "on", "off", "1.0", "1e2", "0x1A", ".nan", ".inf"],
    )
    def test_yaml_keyword_filenames(self, filename):
        tree = {"name": "root", "type": "directory", "children": [{"name": filename, "type": "file", "content": "x\n"}]}
        parsed, _raw = self._roundtrip(tree)
        child = parsed["children"][0]
        assert child["name"] == filename, f"Filename '{filename}' was not preserved as string"
        assert isinstance(child["name"], str)

    def test_control_chars_in_content(self):
        content = "line1\rline2\x00line3\x85line4\n"
        tree = {"name": "p", "type": "directory", "children": [{"name": "c.txt", "type": "file", "content": content}]}
        parsed, _ = self._roundtrip(tree)
        assert parsed["children"][0]["content"] == content

    def test_special_chars_in_filename(self):
        for name in ['file "quoted".txt', "file\\back.txt", "file: colon.txt", "file\nnewline.txt"]:
            tree = {"name": "p", "type": "directory", "children": [{"name": name, "type": "file", "content": "x\n"}]}
            parsed, _ = self._roundtrip(tree)
            assert parsed["children"][0]["name"] == name

    def test_deeply_nested_structure(self):
        node = {"name": "leaf.txt", "type": "file", "content": "found\n"}
        for i in range(20):
            node = {"name": f"d{i}", "type": "directory", "children": [node]}
        tree = {"name": "root", "type": "directory", "children": [node]}
        parsed, _ = self._roundtrip(tree)

        current = parsed
        for i in range(19, -1, -1):
            current = current["children"][0]
            assert current["name"] == f"d{i}"
        assert current["children"][0]["name"] == "leaf.txt"
        assert current["children"][0]["content"] == "found\n"

    def test_backtick_content(self):
        content = "```python\nprint('hi')\n```\n"
        tree = {"name": "p", "type": "directory", "children": [{"name": "md.txt", "type": "file", "content": content}]}
        parsed, _ = self._roundtrip(tree)
        assert parsed["children"][0]["content"] == content

    def test_unicode_line_separators(self):
        content = "a\u2028b\u2029c\n"
        tree = {"name": "p", "type": "directory", "children": [{"name": "u.txt", "type": "file", "content": content}]}
        parsed, _ = self._roundtrip(tree)
        assert parsed["children"][0]["content"] == content

    def test_empty_directory_no_children_key(self):
        tree = {"name": "empty", "type": "directory", "children": []}
        parsed, raw = self._roundtrip(tree)
        assert "children" not in raw or parsed.get("children") is None or parsed.get("children") == []
