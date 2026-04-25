from __future__ import annotations

from typing import TYPE_CHECKING

from .git import GitError
from .pipeline import build_diff_context

if TYPE_CHECKING:
    from .project_graph import ProjectGraph

__all__ = ["GitError", "ProjectGraph", "build_diff_context", "build_project_graph"]


def build_project_graph(*args, **kwargs):  # type: ignore[no-untyped-def]
    from .project_graph import build_project_graph as _build

    return _build(*args, **kwargs)
