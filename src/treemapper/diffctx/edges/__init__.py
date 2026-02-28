from __future__ import annotations

import logging
from collections.abc import Callable
from pathlib import Path
from typing import TYPE_CHECKING

from ..types import FragmentId
from .base import EdgeBuilder, EdgeDict
from .config import get_config_builders
from .document import get_document_builders
from .history import get_history_builders
from .semantic import get_semantic_builders
from .similarity import get_similarity_builders
from .structural import get_structural_builders

if TYPE_CHECKING:
    from ..types import Fragment

EdgeCategories = dict[tuple[FragmentId, FragmentId], str]

_BUILDER_CATEGORIES: list[tuple[str, Callable[[], list[type[EdgeBuilder]]]]] = [
    ("semantic", get_semantic_builders),
    ("structural", get_structural_builders),
    ("config", get_config_builders),
    ("document", get_document_builders),
    ("similarity", get_similarity_builders),
    ("history", get_history_builders),
]


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


_EXPENSIVE_CATEGORIES = frozenset({"similarity", "history"})


def collect_all_edges(
    fragments: list[Fragment],
    repo_root: Path | None = None,
    skip_expensive: bool = False,
) -> tuple[EdgeDict, EdgeCategories]:
    all_edges: EdgeDict = {}
    edge_categories: EdgeCategories = {}
    for category, get_builders in _BUILDER_CATEGORIES:
        if skip_expensive and category in _EXPENSIVE_CATEGORIES:
            logging.debug("diffctx: skipping %s edge builders (skip_expensive=True)", category)
            continue
        for cls in get_builders():
            builder = cls()
            cat = builder.category or category
            for (src, dst), weight in builder.build(fragments, repo_root).items():
                if weight > all_edges.get((src, dst), 0.0):
                    all_edges[(src, dst)] = weight
                    edge_categories[(src, dst)] = cat
    return all_edges, edge_categories


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
    "EdgeCategories",
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
