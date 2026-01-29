from __future__ import annotations

from .anchor import AnchorLinkEdgeBuilder
from .citation import CitationEdgeBuilder
from .structure import DocumentStructureEdgeBuilder


def get_document_builders() -> list[type]:
    return [
        DocumentStructureEdgeBuilder,
        AnchorLinkEdgeBuilder,
        CitationEdgeBuilder,
    ]


__all__ = [
    "AnchorLinkEdgeBuilder",
    "CitationEdgeBuilder",
    "DocumentStructureEdgeBuilder",
    "get_document_builders",
]
