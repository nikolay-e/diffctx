"""Path admission for the MCP tools.

Two rules the messages here follow, not only the checks:

- **Resolve before deciding, report before resolving.** Every check runs on the
  fully resolved path so no `..` or symlink survives into a comparison, but an
  error only ever echoes the string the caller supplied. The resolved form is
  derived knowledge about the filesystem — a symlink's target is exactly the
  thing a caller who was refused should not learn.
- **No absolute paths and no tracebacks in a payload.** These messages reach a
  model, which may relay them, so they say what was wrong and nothing about
  where this process is running (#147).
"""

from __future__ import annotations

import os
from pathlib import Path


def _check_allowed(path: Path, as_given: str) -> None:
    allowed = os.environ.get("DIFFCTX_ALLOWED_PATHS")
    if not allowed:
        return
    allowed_paths = [Path(p).resolve() for p in allowed.split(os.pathsep) if p]
    if not any(path.is_relative_to(a) for a in allowed_paths):
        # Neither the resolved path nor the allowlist itself: the refusal is the
        # answer, and naming the permitted roots hands a caller the map it was
        # just denied.
        raise ValueError(f"Path is outside the roots this server is allowed to read: {as_given}")


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
        raise ValueError(f"Not a git repository: {repo_path} (no .git in it or any parent directory)")
    _check_allowed(repo_root, repo_path)
    return repo_root


def validate_dir_path(dir_path: str) -> Path:
    path = Path(dir_path).resolve()
    if not path.is_dir():
        raise ValueError(f"Not a directory: {dir_path}")
    _check_allowed(path, dir_path)
    return path
