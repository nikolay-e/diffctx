from __future__ import annotations

import os
from pathlib import Path


def _check_allowed(path: Path) -> None:
    allowed = os.environ.get("DIFFCTX_ALLOWED_PATHS")
    if not allowed:
        return
    allowed_paths = [Path(p).resolve() for p in allowed.split(os.pathsep) if p]
    if not any(path.is_relative_to(a) for a in allowed_paths):
        raise ValueError(f"Path not in allowed paths: {path}")


def _is_bare_repo_dir(path: Path) -> bool:
    return (path / "HEAD").is_file() and (path / "objects").is_dir() and (path / "refs").is_dir()


def _find_repo_root(path: Path) -> Path | None:
    current = path
    while True:
        if (current / ".git").exists() or _is_bare_repo_dir(current):
            return current
        parent = current.parent
        if parent == current:
            return None
        current = parent


def validate_repo_path(repo_path: str) -> Path:
    path = Path(repo_path).resolve()
    if not path.is_dir():
        raise ValueError(f"Not a directory: {repo_path}")
    repo_root = _find_repo_root(path)
    if repo_root is None:
        raise ValueError(
            f"Not a git repository (checked {path} and its parent directories up to the filesystem root): {repo_path}"
        )
    _check_allowed(repo_root)
    # Re-validate after resolve to prevent symlink-swap TOCTOU.
    _check_allowed(repo_root.resolve())
    return repo_root


def validate_dir_path(dir_path: str) -> Path:
    path = Path(dir_path).resolve()
    if not path.is_dir():
        raise ValueError(f"Not a directory: {dir_path}")
    _check_allowed(path)
    # Re-validate after resolve to prevent symlink-swap TOCTOU.
    _check_allowed(path.resolve())
    return path
