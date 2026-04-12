from __future__ import annotations

import os
from pathlib import Path


def validate_repo_path(repo_path: str) -> Path:
    path = Path(repo_path).resolve()
    if not path.is_dir():
        raise ValueError(f"Not a directory: {repo_path}")
    if not (path / ".git").exists() and not (path / ".git").is_file():
        raise ValueError(f"Not a git repository: {repo_path}")
    allowed = os.environ.get("TREEMAPPER_ALLOWED_PATHS")
    if allowed:
        allowed_paths = [Path(p).resolve() for p in allowed.split(":") if p]
        if not any(path.is_relative_to(a) for a in allowed_paths):
            raise ValueError(f"Repository path not in allowed paths: {repo_path}")
    return path
