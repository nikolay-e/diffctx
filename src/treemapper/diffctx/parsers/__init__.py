from __future__ import annotations

import logging
from pathlib import Path

from ..types import Fragment
from .base import FragmentationStrategy
from .config import ConfigStrategy, RuamelYamlStrategy
from .generic import GenericStrategy
from .html import HTMLStrategy
from .kubernetes import KubernetesYamlStrategy
from .markdown import MistuneMarkdownStrategy, RegexMarkdownStrategy
from .python import PythonAstStrategy
from .text import ParagraphStrategy, PySBDTextStrategy


def _make_tree_sitter_strategy() -> object | None:
    try:
        from .tree_sitter import TreeSitterStrategy

        return TreeSitterStrategy()
    except ImportError:
        return None


class FragmentationEngine:
    def __init__(self) -> None:
        self._strategies: list[FragmentationStrategy] = []
        self._initialize_strategies()

    def _initialize_strategies(self) -> None:
        strategies: list[object] = [
            _make_tree_sitter_strategy(),
            PythonAstStrategy(),
            MistuneMarkdownStrategy(),
            RegexMarkdownStrategy(),
            HTMLStrategy(),
            KubernetesYamlStrategy(),
            RuamelYamlStrategy(),
            ConfigStrategy(),
            PySBDTextStrategy(),
            ParagraphStrategy(),
            GenericStrategy(),
        ]

        for s in strategies:
            if isinstance(s, FragmentationStrategy):
                self._strategies.append(s)

        self._strategies.sort(key=lambda s: -s.priority)

    def fragment(self, path: Path, content: str) -> list[Fragment]:
        for strategy in self._strategies:
            if strategy.can_handle(path, content):
                try:
                    result = strategy.fragment(path, content)
                    if result:
                        return result
                except Exception as e:
                    logging.warning("Strategy %s failed for %s: %s", type(strategy).__name__, path, e)
                    continue

        return GenericStrategy().fragment(path, content)


_ENGINE = FragmentationEngine()


def fragment_file(path: Path, content: str) -> list[Fragment]:
    return _ENGINE.fragment(path, content)


def enclosing_fragment(fragments: list[Fragment], line_no: int) -> Fragment | None:
    candidates = [f for f in fragments if f.start_line <= line_no <= f.end_line]
    if not candidates:
        return None
    return min(candidates, key=lambda f: (f.line_count, f.start_line))


__all__ = [
    "ConfigStrategy",
    "FragmentationEngine",
    "FragmentationStrategy",
    "GenericStrategy",
    "HTMLStrategy",
    "KubernetesYamlStrategy",
    "MistuneMarkdownStrategy",
    "ParagraphStrategy",
    "PySBDTextStrategy",
    "PythonAstStrategy",
    "RegexMarkdownStrategy",
    "RuamelYamlStrategy",
    "enclosing_fragment",
    "fragment_file",
]
