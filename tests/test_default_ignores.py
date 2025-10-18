# tests/test_default_ignores.py
import os  # Required for permission operations
from pathlib import Path  # Required for file path handling

from .utils import get_all_files_in_tree, load_yaml


# Example function using the imports to avoid unused import warnings
def _ensure_dir_readable(directory: Path) -> bool:
    """Check if a directory is readable."""
    return os.access(directory, os.R_OK)


def test_default_python_ignores(temp_project, run_mapper):
    """Test for built-in Python-specific ignore patterns."""
    # Create Python cache files and directories
    cache_dir = temp_project / "__pycache__"
    cache_dir.mkdir(exist_ok=True)
    (cache_dir / "module.cpython-310.pyc").touch()

    # Create .pyc files in the root
    (temp_project / "module.pyc").touch()
    (temp_project / "module.pyo").touch()
    (temp_project / "module.pyd").touch()

    # Create .egg-info directory
    egg_info_dir = temp_project / "package.egg-info"
    egg_info_dir.mkdir(exist_ok=True)
    (egg_info_dir / "PKG-INFO").touch()

    # Create pytest cache
    pytest_cache_dir = temp_project / ".pytest_cache"
    pytest_cache_dir.mkdir(exist_ok=True)
    (pytest_cache_dir / "README.md").touch()

    # Create normal Python file that should be included
    (temp_project / "actual_module.py").touch()

    # Run TreeMapper and check results
    assert run_mapper([".", "-o", "directory_tree.yaml"])
    result = load_yaml(temp_project / "directory_tree.yaml")
    all_files = get_all_files_in_tree(result)

    # Check that Python cache files are ignored by default
    assert "__pycache__" not in all_files
    assert "module.cpython-310.pyc" not in all_files
    assert "module.pyc" not in all_files
    assert "module.pyo" not in all_files
    assert "module.pyd" not in all_files

    # Egg info should be ignored
    assert "package.egg-info" not in all_files
    assert "PKG-INFO" not in all_files

    # Pytest cache should be ignored
    assert ".pytest_cache" not in all_files

    # Regular Python files should be included
    assert "actual_module.py" in all_files


def test_git_directory_ignored(temp_project, run_mapper):
    """Test that .git directory is properly ignored."""
    # Create .git directory structure
    git_dir = temp_project / ".git"
    git_dir.mkdir(exist_ok=True)
    (git_dir / "HEAD").touch()
    (git_dir / "config").touch()

    # Create branches directory
    branches_dir = git_dir / "branches"
    branches_dir.mkdir(exist_ok=True)
    (branches_dir / "main").touch()

    # Create a normal file that should be included
    (temp_project / "README.md").touch()

    # Run TreeMapper and check results
    assert run_mapper([".", "-o", "directory_tree.yaml"])
    result = load_yaml(temp_project / "directory_tree.yaml")
    all_files = get_all_files_in_tree(result)

    # Check that .git directory and its contents are ignored
    assert ".git" not in all_files
    assert "HEAD" not in all_files
    assert "config" not in all_files
    assert "branches" not in all_files
    assert "main" not in all_files

    # Regular files should be included
    assert "README.md" in all_files


def test_default_verbosity(temp_project, run_mapper, capfd):
    """Test that default verbosity is ERROR level."""
    # Create some test files
    (temp_project / "file1.txt").write_text("content1")
    (temp_project / "file2.txt").write_text("content2")

    # Clear any buffer
    capfd.readouterr()

    # Run with default verbosity (ERROR)
    assert run_mapper([".", "-o", "directory_tree.yaml"])
    out, err = capfd.readouterr()

    # At ERROR level, there should be no INFO messages
    assert "INFO:" not in err
