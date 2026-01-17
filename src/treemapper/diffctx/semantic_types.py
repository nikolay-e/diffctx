from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class SemanticInfo:
    defines: frozenset[str]
    references: frozenset[str]
    calls: frozenset[str]
    type_refs: frozenset[str]


EMPTY_SEMANTIC_INFO = SemanticInfo(frozenset(), frozenset(), frozenset(), frozenset())


@dataclass(frozen=True)
class JsSemanticInfo(SemanticInfo):
    imports: frozenset[str] = frozenset()
    exports: frozenset[str] = frozenset()


EMPTY_JS_SEMANTIC_INFO = JsSemanticInfo(frozenset(), frozenset(), frozenset(), frozenset(), frozenset(), frozenset())
