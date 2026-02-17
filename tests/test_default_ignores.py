# tests/test_default_ignores.py
import sys

from .utils import get_all_files_in_tree, load_yaml


def _get_pycache_filename(module_name: str) -> str:
    py_version = f"cpython-{sys.version_info.major}{sys.version_info.minor}"
    return f"{module_name}.{py_version}.pyc"


def test_default_python_ignores(temp_project, run_mapper):
    cache_dir = temp_project / "__pycache__"
    cache_dir.mkdir(exist_ok=True)
    pycache_file = _get_pycache_filename("module")
    (cache_dir / pycache_file).touch()

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

    # Run treemapper and check results
    assert run_mapper([".", "-o", "directory_tree.yaml"])
    result = load_yaml(temp_project / "directory_tree.yaml")
    all_files = get_all_files_in_tree(result)

    # Check that Python cache files are ignored by default
    assert "__pycache__" not in all_files
    assert pycache_file not in all_files
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

    # Run treemapper and check results
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


def test_default_directory_ignores(temp_project, run_mapper):
    for dir_name in ["node_modules", "venv", ".venv"]:
        d = temp_project / dir_name
        d.mkdir(exist_ok=True)
        (d / "file.txt").touch()

    for cache_name in [".mypy_cache", ".ruff_cache"]:
        d = temp_project / cache_name
        d.mkdir(exist_ok=True)
        (d / "data.json").touch()

    (temp_project / "real_code.py").touch()

    assert run_mapper([".", "-o", "directory_tree.yaml"])
    result = load_yaml(temp_project / "directory_tree.yaml")
    all_files = get_all_files_in_tree(result)

    for ignored in ["node_modules", "venv", ".venv", ".mypy_cache", ".ruff_cache"]:
        assert ignored not in all_files

    assert "real_code.py" in all_files
