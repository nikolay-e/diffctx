# tests/conftest.py
import logging
import os
import subprocess
import sys
from pathlib import Path

import pytest

# Add project root/src to PYTHONPATH for subprocess tests
PROJECT_ROOT = Path(__file__).parent.parent
SRC_DIR = PROJECT_ROOT / "src"

# Constants for ignore file names
GITIGNORE = ".gitignore"
TREEMAPPERIGNORE = ".treemapperignore"

# WSL detection with proper file handle cleanup (shared across test files)
_PROC_VERSION = Path("/proc/version")
IS_WSL = _PROC_VERSION.exists() and "microsoft" in _PROC_VERSION.read_text(errors="ignore").lower()


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
    (temp_dir / GITIGNORE).write_text("*.pyc\n__pycache__/\n", encoding="utf-8")
    (temp_dir / TREEMAPPERIGNORE).write_text("output/\n.git/\n", encoding="utf-8")
    yield temp_dir


# --- Фикстура для запуска маппера ---
@pytest.fixture
def run_mapper(monkeypatch, temp_project):
    """Helper to run treemapper with given args."""

    def _run(args):
        """Runs the main function with patched CWD and sys.argv."""
        with monkeypatch.context() as m:
            m.chdir(temp_project)
            m.setattr(sys, "argv", ["treemapper", *args])
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


# --- Helper for running treemapper as subprocess ---
def run_treemapper_subprocess(args, cwd=None, **kwargs):
    """Run treemapper as a subprocess with proper environment setup.

    Args:
        args: Command line arguments to pass to treemapper
        cwd: Working directory for the subprocess
        **kwargs: Additional arguments to pass to subprocess.run

    Returns:
        CompletedProcess object
    """
    command = [sys.executable, "-m", "treemapper", *args]

    # Ensure subprocess can find the treemapper module
    env = os.environ.copy()
    pythonpath = str(SRC_DIR)
    if "PYTHONPATH" in env:
        pythonpath = f"{pythonpath}{os.pathsep}{env['PYTHONPATH']}"
    env["PYTHONPATH"] = pythonpath

    # Merge with any env provided in kwargs
    if "env" in kwargs:
        env.update(kwargs["env"])
    kwargs["env"] = env

    # Set default values for common parameters
    if "capture_output" not in kwargs:
        kwargs["capture_output"] = True
    if "text" not in kwargs:
        kwargs["text"] = True
    if "encoding" not in kwargs:
        kwargs["encoding"] = "utf-8"
    if "errors" not in kwargs:
        kwargs["errors"] = "replace"

    return subprocess.run(command, cwd=cwd, **kwargs)


# ---> НАЧАЛО: Перенесенная фикстура set_perms <---
@pytest.fixture
def set_perms(request):
    """Fixture to temporarily set file/directory permissions (non-Windows)."""
    original_perms = {}
    paths_changed = []

    def _set_perms(path: Path, perms: int):
        if sys.platform == "win32":
            pytest.skip("Permission tests skipped on Windows.")

        # Check if running in WSL environment
        is_wsl = False
        try:
            with open("/proc/version") as f:
                if "microsoft" in f.read().lower():
                    is_wsl = True
        except FileNotFoundError:
            pass

        # Skip tests on Windows paths in WSL
        if is_wsl and "/mnt/" in str(path):
            pytest.skip(f"Permission tests skipped on Windows-mounted paths in WSL: {path}")

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


# --- New fixtures for test modernization ---


@pytest.fixture
def project_builder(tmp_path):
    """Builder pattern for creating test project structures."""

    class ProjectBuilder:
        def __init__(self, base_path: Path):
            self.root = base_path / "treemapper_test_project"
            self.root.mkdir()

        def add_file(self, path: str, content: str = "") -> Path:
            file_path = self.root / path
            file_path.parent.mkdir(parents=True, exist_ok=True)
            file_path.write_text(content, encoding="utf-8")
            return file_path

        def add_binary(self, path: str, content: bytes = b"\x00\x01\x02") -> Path:
            file_path = self.root / path
            file_path.parent.mkdir(parents=True, exist_ok=True)
            file_path.write_bytes(content)
            return file_path

        def add_dir(self, path: str) -> Path:
            dir_path = self.root / path
            dir_path.mkdir(parents=True, exist_ok=True)
            return dir_path

        def add_gitignore(self, patterns: list[str], subdir: str = "") -> Path:
            path = self.root / subdir / GITIGNORE if subdir else self.root / GITIGNORE
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("\n".join(patterns) + "\n", encoding="utf-8")
            return path

        def add_treemapperignore(self, patterns: list[str]) -> Path:
            path = self.root / TREEMAPPERIGNORE
            path.write_text("\n".join(patterns) + "\n", encoding="utf-8")
            return path

        def create_nested(self, depth: int, files_per_level: int = 1) -> None:
            current = self.root
            for i in range(depth):
                current = current / f"level{i}"
                current.mkdir(exist_ok=True)
                for j in range(files_per_level):
                    (current / f"file{j}.txt").write_text(f"Content {i}-{j}")

    return ProjectBuilder(tmp_path)


@pytest.fixture
def cli_runner(temp_project):
    """Simplified CLI runner with automatic success assertion."""

    def _run(args, cwd=None, expect_success=True):
        result = run_treemapper_subprocess(args, cwd=cwd or temp_project)
        if expect_success:
            assert result.returncode == 0, f"CLI failed with stderr: {result.stderr}"
        return result

    return _run


@pytest.fixture
def run_and_verify(run_mapper, temp_project):
    """Run mapper and verify tree structure."""
    from tests.utils import get_all_files_in_tree, load_yaml

    def _run(
        args=None,
        expected_files=None,
        excluded_files=None,
        output_name="output.yaml",
    ):
        output_path = temp_project / output_name
        full_args = ["."] + (args or []) + ["-o", str(output_path)]
        success = run_mapper(full_args)
        assert success, f"Mapper failed with args: {full_args}"

        result = load_yaml(output_path)
        all_files = get_all_files_in_tree(result)

        if expected_files:
            for f in expected_files:
                assert f in all_files, f"Expected file '{f}' not found in tree"
        if excluded_files:
            for f in excluded_files:
                assert f not in all_files, f"File '{f}' should be excluded from tree"

        return result

    return _run
