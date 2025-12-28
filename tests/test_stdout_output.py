# tests/test_stdout_output.py
import sys

import pytest
import yaml

from .conftest import run_treemapper_subprocess
from .utils import load_yaml


@pytest.mark.parametrize(
    "output_flag",
    [
        ["-o", "-"],
        ["--output-file", "-"],
    ],
)
def test_stdout_output_flags(temp_project, output_flag):
    (temp_project / "test.txt").write_text("test content", encoding="utf-8")
    (temp_project / "subdir").mkdir()
    (temp_project / "subdir" / "file.py").write_text("print('hello')", encoding="utf-8")

    result = run_treemapper_subprocess([".", *output_flag], cwd=temp_project)

    assert result.returncode == 0
    assert result.stdout.strip() != ""

    tree_data = yaml.safe_load(result.stdout)
    assert tree_data["type"] == "directory"
    assert tree_data["name"] == temp_project.name

    children_names = [child["name"] for child in tree_data.get("children", [])]
    assert "test.txt" in children_names
    assert "subdir" in children_names

    assert not (temp_project / "directory_tree.yaml").exists()


def test_stdout_output_preserves_stderr_logging(temp_project):
    result = run_treemapper_subprocess([".", "-o", "-", "--log-level", "info"], cwd=temp_project)

    assert result.returncode == 0
    assert result.stdout.strip() != ""
    assert "INFO" in result.stderr
    assert "Directory tree written to stdout" in result.stderr

    tree_data = yaml.safe_load(result.stdout)
    assert tree_data["type"] == "directory"


def test_stdout_output_with_file_content(temp_project):
    test_content = "def hello():\n    print('Hello, World!')\n"
    (temp_project / "hello.py").write_text(test_content, encoding="utf-8")

    result = run_treemapper_subprocess([".", "-o", "-"], cwd=temp_project)
    assert result.returncode == 0

    tree_data = yaml.safe_load(result.stdout)

    hello_file = next((c for c in tree_data.get("children", []) if c.get("name") == "hello.py"), None)
    assert hello_file is not None
    assert hello_file["type"] == "file"
    assert hello_file["content"] == test_content


def test_stdout_output_respects_ignore_patterns(temp_project):
    (temp_project / "include.txt").write_text("included", encoding="utf-8")
    (temp_project / "exclude.txt").write_text("excluded", encoding="utf-8")

    ignore_file = temp_project / "custom.ignore"
    ignore_file.write_text("exclude.txt\n")

    result = run_treemapper_subprocess([".", "-o", "-", "-i", str(ignore_file)], cwd=temp_project)
    assert result.returncode == 0

    tree_data = yaml.safe_load(result.stdout)
    children_names = [child["name"] for child in tree_data.get("children", [])]
    assert "include.txt" in children_names
    assert "exclude.txt" not in children_names


def test_stdout_output_with_special_characters(temp_project):
    special_content = "Special chars: é ñ ü 中文 🚀\n"
    (temp_project / "special_chars.txt").write_text(special_content, encoding="utf-8")

    result = run_treemapper_subprocess([".", "-o", "-"], cwd=temp_project)
    assert result.returncode == 0

    tree_data = yaml.safe_load(result.stdout)
    special_file = next((c for c in tree_data.get("children", []) if c.get("name") == "special_chars.txt"), None)
    assert special_file is not None
    assert special_file["content"] == special_content


def test_stdout_output_large_tree(temp_project):
    for i in range(3):
        subdir = temp_project / f"dir_{i}"
        subdir.mkdir()
        for j in range(3):
            (subdir / f"file_{j}.txt").write_text(f"Content {i}-{j}", encoding="utf-8")
            nested = subdir / f"nested_{j}"
            nested.mkdir()
            (nested / "deep.txt").write_text("Deep content", encoding="utf-8")

    result = run_treemapper_subprocess([".", "-o", "-"], cwd=temp_project)
    assert result.returncode == 0

    tree_data = yaml.safe_load(result.stdout)
    assert tree_data["type"] == "directory"
    assert len(tree_data.get("children", [])) >= 3

    def count_nodes(node):
        count = 1
        for child in node.get("children", []):
            count += count_nodes(child)
        return count

    assert count_nodes(tree_data) > 30


def test_stdout_vs_file_output_consistency(temp_project):
    (temp_project / "test.py").write_text("import os\n", encoding="utf-8")
    (temp_project / "data").mkdir()
    (temp_project / "data" / "info.txt").write_text("data", encoding="utf-8")

    result_stdout = run_treemapper_subprocess([".", "-o", "-"], cwd=temp_project)
    assert result_stdout.returncode == 0
    stdout_data = yaml.safe_load(result_stdout.stdout)

    output_file = temp_project / "test_output.yaml"
    result_file = run_treemapper_subprocess([".", "-o", str(output_file)], cwd=temp_project)
    assert result_file.returncode == 0
    file_data = load_yaml(output_file)

    assert stdout_data == file_data


def test_stdout_output_error_handling(temp_project):
    result = run_treemapper_subprocess(["non_existent_dir", "-o", "-"], cwd=temp_project)
    assert result.returncode != 0
    assert "Error:" in result.stderr
    assert result.stdout.strip() == ""


def test_stdout_output_with_permission_errors(temp_project, set_perms):
    if sys.platform == "win32":
        pytest.skip("Permission tests skipped on Windows.")

    (temp_project / "readable.txt").write_text("can read", encoding="utf-8")
    unreadable = temp_project / "unreadable.txt"
    unreadable.write_text("cannot read", encoding="utf-8")
    set_perms(unreadable, 0o000)

    result = run_treemapper_subprocess([".", "-o", "-", "--log-level", "debug"], cwd=temp_project)
    assert result.returncode == 0

    tree_data = yaml.safe_load(result.stdout)
    files = {child["name"]: child for child in tree_data.get("children", [])}

    assert "readable.txt" in files
    assert "unreadable.txt" in files
    assert files["readable.txt"]["content"] == "can read\n"
    assert files["unreadable.txt"]["content"] == "<unreadable content>\n"
    assert "Could not read" in result.stderr
