# tests/conftest.py
import logging
import os
import sys
from pathlib import Path

import pytest


# --- Фикстура для создания временного проекта ---
@pytest.fixture
def temp_project(tmp_path):
    """Create a temporary project structure for testing."""
    temp_dir = tmp_path / "treemapper_test_project"
    temp_dir.mkdir()
    (temp_dir / "src").mkdir()
    (temp_dir / "src" / "main.py").write_text("def main():\n    print('hello')\n", encoding="utf-8")
    (temp_dir / "src" / "test.py").write_text("def test():\n    pass\n", encoding="utf-8")
    (temp_dir / "docs").mkdir()
    (temp_dir / "docs" / "readme.md").write_text("# Documentation\n", encoding="utf-8")
    (temp_dir / "output").mkdir()
    (temp_dir / ".git").mkdir()
    (temp_dir / ".git" / "config").write_text("git config file", encoding="utf-8")
    (temp_dir / ".gitignore").write_text("*.pyc\n__pycache__/\n", encoding="utf-8")
    (temp_dir / ".treemapperignore").write_text("output/\n.git/\n", encoding="utf-8")
    yield temp_dir


# --- Фикстура для запуска маппера ---
@pytest.fixture
def run_mapper(monkeypatch, temp_project):
    """Helper to run treemapper with given args."""

    def _run(args):
        """Runs the main function with patched CWD and sys.argv."""
        with monkeypatch.context() as m:
            m.chdir(temp_project)
            m.setattr(sys, "argv", ["treemapper"] + args)
            try:
                from treemapper.treemapper import main

                main()
                return True
            except SystemExit as e:
                if e.code != 0:
                    print(f"SystemExit caught with code: {e.code}")
                    return False
                return True
            except Exception as e:
                print(f"Caught unexpected exception in run_mapper: {e}")

                return False

    return _run


# ---> НАЧАЛО: Перенесенная фикстура set_perms <---
@pytest.fixture
def set_perms(request):
    """Fixture to temporarily set file/directory permissions (non-Windows)."""
    original_perms = {}
    paths_changed = []

    def _set_perms(path: Path, perms: int):
        if sys.platform == "win32":
            pytest.skip("Permission tests skipped on Windows.")
        if not path.exists():
            pytest.skip(f"Path does not exist, cannot set permissions: {path}")
        try:
            original_perms[path] = path.stat().st_mode
            paths_changed.append(path)
            os.chmod(path, perms)
            logging.debug(f"Set permissions {oct(perms)} for {path}")
        except OSError as e:
            pytest.skip(f"Could not set permissions on {path}: {e}. Skipping test.")

    yield _set_perms

    logging.debug(f"Cleaning up permissions for: {paths_changed}")
    for path in paths_changed:

        if path.exists() and path in original_perms:
            try:

                if original_perms[path] is not None:
                    os.chmod(path, original_perms[path])
                    logging.debug(f"Restored permissions for {path}")
                else:
                    logging.warning(f"Original permissions for {path} were None, not restoring.")
            except OSError as e:

                logging.warning(f"Could not restore permissions for {path}: {e}")


# ---> КОНЕЦ: Перенесенная фикстура set_perms <---
