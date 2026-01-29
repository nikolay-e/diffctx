from __future__ import annotations

from .lexical import LexicalEdgeBuilder, clamp_lexical_weight


def get_similarity_builders() -> list[type]:
    return [
        LexicalEdgeBuilder,
    ]


__all__ = [
    "LexicalEdgeBuilder",
    "clamp_lexical_weight",
    "get_similarity_builders",
]
