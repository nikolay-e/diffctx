from __future__ import annotations

from .cochange import CochangeEdgeBuilder


def get_history_builders() -> list[type]:
    return [
        CochangeEdgeBuilder,
    ]


__all__ = [
    "CochangeEdgeBuilder",
    "get_history_builders",
]
