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


def validate_repo_path(repo_path: str) -> Path:
    path = Path(repo_path).resolve()
    if not path.is_dir():
        raise ValueError(f"Not a directory: {repo_path}")
    if not (path / ".git").exists() and not (path / ".git").is_file():
        raise ValueError(f"Not a git repository: {repo_path}")
    _check_allowed(path)
    # Re-validate after resolve to prevent symlink-swap TOCTOU.
    _check_allowed(path.resolve())
    return path


def validate_dir_path(dir_path: str) -> Path:
    path = Path(dir_path).resolve()
    if not path.is_dir():
        raise ValueError(f"Not a directory: {dir_path}")
    _check_allowed(path)
    # Re-validate after resolve to prevent symlink-swap TOCTOU.
    _check_allowed(path.resolve())
    return path
