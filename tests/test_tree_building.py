from treemapper.treemapper import build_tree, load_pathspec
import os
import platform
import sys
from pathlib import Path

import pytest

# Add the src directory to the Python path
sys.path.insert(
    0,
    os.path.abspath(
        os.path.join(
            os.path.dirname(__file__),
            '..',
            'src')))


@pytest.fixture
def temp_directory(tmp_path):
    (tmp_path / "dir1").mkdir()
    (tmp_path / "dir1" / "file1.txt").write_text("Content of file1")
    (tmp_path / "dir2").mkdir()
    (tmp_path / "dir2" / "file2.py").write_text("print('Hello, World!')")
    (tmp_path / ".gitignore").write_text("*.log\n")
    (tmp_path / ".git").mkdir()
    return tmp_path


def test_build_tree(temp_directory):
    combined_patterns = [".git/", "*.log"]
    combined_spec = load_pathspec(combined_patterns)
    gitignore_specs = {temp_directory: load_pathspec(["*.log"])}
    tree = build_tree(
        temp_directory,
        temp_directory,
        combined_spec,
        gitignore_specs)
    assert len(tree) == 3  # dir1, dir2, and .gitignore (but not .git)
    assert any(node["name"] == "dir1" for node in tree)
    assert any(node["name"] == "dir2" for node in tree)
    assert any(node["name"] == ".gitignore" for node in tree)
    assert not any(node["name"] == ".git" for node in tree)


def test_build_tree_with_ignore_patterns(temp_directory):
    ignore_patterns = [".git/", "*.txt", "dir1/"]
    combined_spec = load_pathspec(ignore_patterns)
    gitignore_specs = {temp_directory: load_pathspec(["*.log"])}
    tree = build_tree(
        temp_directory,
        temp_directory,
        combined_spec,
        gitignore_specs)

    assert not any(node["name"] == "dir1" for node in tree)
    assert not any(node["name"] == "file1.txt" for node in tree)
    assert not any(node["name"] == ".git" for node in tree)
    assert any(node["name"] == "dir2" for node in tree)
    assert any(node["name"] == ".gitignore" for node in tree)

    # Check that symlinks are handled correctly (ignored in this case)
    if platform.system() != "Windows":
        assert not any(node["name"] == "symlink" for node in tree)


def test_permission_error_handling(monkeypatch, tmp_path):
    def mock_iterdir(_):
        raise PermissionError("Permission denied")
    monkeypatch.setattr(Path, "iterdir", mock_iterdir)
    tree = build_tree(tmp_path, tmp_path, load_pathspec([]), {})
    assert tree == []  # Tree should be empty due to the permission error


def test_deep_directory_structure(tmp_path):
    # Adjust directory depth based on platform
    max_depth = 30 if platform.system() == "Windows" else 100

    current_dir = tmp_path
    if platform.system() == "Windows":
        # Use '\\?\' to allow long paths in Windows
        current_dir = Path(f"\\\\?\\{tmp_path}")

    for i in range(max_depth):
        current_dir = current_dir / f"dir{i}"
        current_dir.mkdir()

    # Assert that the deep directory structure is built correctly
    tree = build_tree(tmp_path, tmp_path, load_pathspec([]), {})
    assert len(tree) == 1
    assert tree[0]["name"] == "dir0"
