from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING

from .base import EdgeBuilder, EdgeDict
from .config import get_config_builders
from .document import get_document_builders
from .history import get_history_builders
from .semantic import get_semantic_builders
from .similarity import get_similarity_builders
from .structural import get_structural_builders

if TYPE_CHECKING:
    from ..types import Fragment


def get_all_builders() -> list[EdgeBuilder]:
    all_builder_classes = (
        get_config_builders()
        + get_semantic_builders()
        + get_structural_builders()
        + get_document_builders()
        + get_similarity_builders()
        + get_history_builders()
    )
    return [cls() for cls in all_builder_classes]


def collect_all_edges(fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
    all_edges: EdgeDict = {}
    for builder in get_all_builders():
        for (src, dst), weight in builder.build(fragments, repo_root).items():
            all_edges[(src, dst)] = max(all_edges.get((src, dst), 0.0), weight)
    return all_edges


def discover_all_related_files(
    changed_files: list[Path],
    all_candidate_files: list[Path],
    repo_root: Path | None = None,
) -> list[Path]:
    discovered: set[Path] = set()
    for builder in get_all_builders():
        for f in builder.discover_related_files(changed_files, all_candidate_files, repo_root):
            discovered.add(f)
    return list(discovered)


__all__ = [
    "EdgeBuilder",
    "EdgeDict",
    "collect_all_edges",
    "discover_all_related_files",
    "get_all_builders",
    "get_config_builders",
    "get_document_builders",
    "get_history_builders",
    "get_semantic_builders",
    "get_similarity_builders",
    "get_structural_builders",
]
