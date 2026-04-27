# tests/conftest.py
import logging
import os
import subprocess
import sys
from pathlib import Path

import pytest

from tests.framework.pygit2_backend import Pygit2Repo

PROJECT_ROOT = Path(__file__).parent.parent
SRC_DIR = PROJECT_ROOT / "src"

GITIGNORE = ".gitignore"

_PROC_VERSION = Path("/proc/version")
IS_WSL = _PROC_VERSION.exists() and "microsoft" in _PROC_VERSION.read_text(errors="ignore").lower()


@pytest.fixture
def temp_project(tmp_path):
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
    config_dir = temp_dir / ".treemapper"
    config_dir.mkdir()
    (config_dir / "ignore").write_text("output/\n.git/\n", encoding="utf-8")
    yield temp_dir


@pytest.fixture
def run_mapper(monkeypatch, temp_project):
    def _run(args):
        with monkeypatch.context() as m:
            m.chdir(temp_project)
            m.setattr(sys, "argv", ["treemapper", *args])
            try:
                from treemapper.treemapper import main

                main()
                return True
            except SystemExit as e:
                return e.code is None or e.code == 0

    return _run


def run_treemapper_subprocess(args, cwd=None, **kwargs):
    command = [sys.executable, "-m", "treemapper", *args]

    env = os.environ.copy()
    pythonpath = str(SRC_DIR)
    if "PYTHONPATH" in env:
        pythonpath = f"{pythonpath}{os.pathsep}{env['PYTHONPATH']}"
    env["PYTHONPATH"] = pythonpath

    if "env" in kwargs:
        env.update(kwargs["env"])
    kwargs["env"] = env

    if "capture_output" not in kwargs:
        kwargs["capture_output"] = True
    if "text" not in kwargs:
        kwargs["text"] = True
    if "encoding" not in kwargs:
        kwargs["encoding"] = "utf-8"
    if "errors" not in kwargs:
        kwargs["errors"] = "replace"

    return subprocess.run(command, cwd=cwd, **kwargs)


def _check_wsl_windows_path(path: Path) -> bool:
    if not IS_WSL:
        return False
    return "/mnt/" in str(path)


def _restore_permissions(paths_changed: list[Path], original_perms: dict[Path, int]) -> None:
    logging.debug(f"Cleaning up permissions for: {paths_changed}")
    for path in paths_changed:
        if not path.exists() or path not in original_perms:
            continue
        orig = original_perms[path]
        if orig is None:
            logging.warning(f"Original permissions for {path} were None, not restoring.")
            continue
        try:
            os.chmod(path, orig)
            logging.debug(f"Restored permissions for {path}")
        except OSError as e:
            logging.warning(f"Could not restore permissions for {path}: {e}")


@pytest.fixture
def set_perms():
    original_perms: dict[Path, int] = {}
    paths_changed: list[Path] = []

    def _set_perms(path: Path, perms: int):
        if sys.platform == "win32":
            pytest.skip("Permission tests skipped on Windows.")
        if _check_wsl_windows_path(path):
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

    _restore_permissions(paths_changed, original_perms)


@pytest.fixture
def project_builder(tmp_path):
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

        def add_treemapper_ignore(self, patterns: list[str]) -> Path:
            config_dir = self.root / ".treemapper"
            config_dir.mkdir(exist_ok=True)
            path = config_dir / "ignore"
            path.write_text("\n".join(patterns) + "\n", encoding="utf-8")
            return path

        def create_nested(self, depth: int, files_per_level: int = 1) -> None:
            current = self.root
            for i in range(depth):
                current = current / f"level{i}"
                current.mkdir(exist_ok=True)
                for j in range(files_per_level):
                    (current / f"file{j}.txt").write_text(f"Content {i}-{j}", encoding="utf-8")

    return ProjectBuilder(tmp_path)


@pytest.fixture
def git_repo(tmp_path):
    repo_path = tmp_path / "git_test_repo"
    Pygit2Repo(repo_path)
    return repo_path
