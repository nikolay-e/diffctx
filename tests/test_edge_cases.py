# tests/test_edge_cases.py
import sys

import pytest

from .utils import get_all_files_in_tree, load_yaml


def test_empty_directory(temp_project, run_mapper):
    """Test: empty directory as input."""
    empty_dir = temp_project / "empty_test_dir"
    empty_dir.mkdir()

    output_path = temp_project / "empty_dir_output.yaml"
    assert run_mapper([str(empty_dir), "-o", str(output_path)])
    result = load_yaml(output_path)

    assert result["name"] == empty_dir.name
    assert result["type"] == "directory"
    assert "children" not in result or not result["children"]


def test_directory_with_only_ignored(temp_project, run_mapper):
    """Test: directory contains only ignored files/folders."""
    ignored_dir = temp_project / "ignored_only_dir"
    ignored_dir.mkdir()
    (ignored_dir / ".DS_Store").touch()
    (ignored_dir / "temp").mkdir()
    (ignored_dir / "temp" / "file.tmp").touch()
    (ignored_dir / ".gitignore").write_text(".DS_Store\ntemp/\n")

    output_path = temp_project / "ignored_only_output.yaml"
    assert run_mapper([str(ignored_dir), "-o", str(output_path)])
    result = load_yaml(output_path)

    assert result["name"] == ignored_dir.name
    assert result["type"] == "directory"
    assert "children" in result and len(result["children"]) == 1
    assert result["children"][0]["name"] == ".gitignore"


def test_filenames_with_special_yaml_chars(temp_project, run_mapper):
    """Test: file names with YAML special characters (manual writer check)."""

    # Basic special characters
    (temp_project / "-startswithdash.txt").touch()
    (temp_project / "quotes'single'.txt").touch()
    (temp_project / "bracket[].txt").touch()
    (temp_project / "curly{}.txt").touch()
    (temp_project / "percent%.txt").touch()
    (temp_project / "ampersand&.txt").touch()

    # YAML reserved words and values
    (temp_project / "true").touch()
    (temp_project / "false").touch()
    (temp_project / "null").touch()
    (temp_project / "yes").touch()
    (temp_project / "no").touch()
    (temp_project / "on").touch()
    (temp_project / "off").touch()

    # Files with quotes that need escaping
    try:
        # Windows doesn't support double quotes in filenames
        (temp_project / 'double"quote.txt').touch()
    except OSError:
        # Fall back to another special character on Windows
        (temp_project / "special@char.txt").touch()

    # Numeric filenames
    (temp_project / "123").touch()
    (temp_project / "0.5").touch()

    output_path = temp_project / "special_chars_output.yaml"
    assert run_mapper([".", "-o", str(output_path)])

    result = load_yaml(output_path)
    all_files = get_all_files_in_tree(result)

    # Check if files were correctly included in the output
    if (temp_project / "-startswithdash.txt").exists():
        assert "-startswithdash.txt" in all_files
    if (temp_project / "quotes'single'.txt").exists():
        assert "quotes'single'.txt" in all_files
    if (temp_project / "bracket[].txt").exists():
        assert "bracket[].txt" in all_files
    if (temp_project / "curly{}.txt").exists():
        assert "curly{}.txt" in all_files
    if (temp_project / "percent%.txt").exists():
        assert "percent%.txt" in all_files
    if (temp_project / "ampersand&.txt").exists():
        assert "ampersand&.txt" in all_files

    # Check YAML reserved words
    assert "true" in all_files
    assert "false" in all_files
    assert "null" in all_files
    assert "yes" in all_files
    assert "no" in all_files
    assert "on" in all_files
    assert "off" in all_files

    # Check quoted/special filenames
    if (temp_project / 'double"quote.txt').exists():
        assert 'double"quote.txt' in all_files
    elif (temp_project / "special@char.txt").exists():
        assert "special@char.txt" in all_files

    # Check numeric filenames
    assert "123" in all_files
    assert "0.5" in all_files


def test_filenames_with_spaces(temp_project, run_mapper):
    (temp_project / "file with spaces.txt").write_text("spaced content")
    subdir = temp_project / "dir with spaces"
    subdir.mkdir()
    (subdir / "nested file.txt").write_text("nested spaced")

    output_path = temp_project / "spaces_output.yaml"
    assert run_mapper([".", "-o", str(output_path)])

    result = load_yaml(output_path)
    all_files = get_all_files_in_tree(result)
    assert "file with spaces.txt" in all_files
    assert "dir with spaces" in all_files
    assert "nested file.txt" in all_files


@pytest.mark.skipif(
    sys.platform == "win32",
    reason="Symlinks require elevated privileges on Windows",
)
def test_broken_symlink_does_not_crash(temp_project, run_mapper):
    link = temp_project / "broken_link.txt"
    link.symlink_to(temp_project / "nonexistent_target.txt")

    output_path = temp_project / "broken_symlink_output.yaml"
    assert run_mapper([".", "-o", str(output_path)])
    result = load_yaml(output_path)
    assert result["type"] == "directory"


def test_output_file_excluded_from_tree(temp_project, run_mapper):
    output_path = temp_project / "tree_output.yaml"
    assert run_mapper([".", "-o", str(output_path)])

    result = load_yaml(output_path)
    all_files = get_all_files_in_tree(result)
    assert "tree_output.yaml" not in all_files


def test_deeply_nested_single_file(temp_project, run_mapper):
    current = temp_project
    depth = 30
    for i in range(depth):
        current = current / f"d{i}"
        current.mkdir()
    (current / "leaf.txt").write_text("deep content")

    output_path = temp_project / "deep_output.yaml"
    assert run_mapper([".", "-o", str(output_path)])

    result = load_yaml(output_path)
    all_files = get_all_files_in_tree(result)
    assert "leaf.txt" in all_files


def test_file_with_no_extension(temp_project, run_mapper):
    (temp_project / "Makefile").write_text("all:\n\techo hello\n")
    (temp_project / "Dockerfile").write_text("FROM alpine\n")
    (temp_project / "LICENSE").write_text("MIT License")

    output_path = temp_project / "noext_output.yaml"
    assert run_mapper([".", "-o", str(output_path)])

    result = load_yaml(output_path)
    all_files = get_all_files_in_tree(result)
    assert "Makefile" in all_files
    assert "Dockerfile" in all_files
    assert "LICENSE" in all_files
