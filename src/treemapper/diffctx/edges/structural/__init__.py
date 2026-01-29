from __future__ import annotations

from .containment import ContainmentEdgeBuilder
from .sibling import SiblingEdgeBuilder
from .test import TestEdgeBuilder


def get_structural_builders() -> list[type]:
    return [
        ContainmentEdgeBuilder,
        TestEdgeBuilder,
        SiblingEdgeBuilder,
    ]


__all__ = [
    "ContainmentEdgeBuilder",
    "SiblingEdgeBuilder",
    "TestEdgeBuilder",
    "get_structural_builders",
]
