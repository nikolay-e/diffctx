from __future__ import annotations

import logging
from collections.abc import Callable
from pathlib import Path
from typing import TYPE_CHECKING

from .types import Fragment, FragmentId

if TYPE_CHECKING:
    from sentence_transformers import SentenceTransformer

_EMBED_WEIGHT = 0.35
_EMBED_MAX_CHARS = 2048
_EMBED_MODEL_NAME = "jinaai/jina-embeddings-v2-base-code"

_TOP_K_NEIGHBORS = 10
_MIN_SIMILARITY = 0.1
_BACKWARD_WEIGHT_FACTOR = 0.7

_EMBED_MODEL: SentenceTransformer | None = None
_EMBED_AVAILABLE: bool | None = None


def _get_embed_model() -> SentenceTransformer | None:
    global _EMBED_MODEL, _EMBED_AVAILABLE
    if _EMBED_AVAILABLE is False:
        return None
    if _EMBED_MODEL is None:
        try:
            from sentence_transformers import SentenceTransformer

            _EMBED_MODEL = SentenceTransformer(_EMBED_MODEL_NAME, trust_remote_code=True)
            _EMBED_AVAILABLE = True
            logging.debug("diffctx: loaded embedding model %s", _EMBED_MODEL_NAME)
        except ImportError:
            logging.debug("diffctx: sentence-transformers not installed, skipping embeddings")
            _EMBED_AVAILABLE = False
            return None
    return _EMBED_MODEL


def _build_embedding_edges(
    fragments: list[Fragment],
    clamp_weight_fn: Callable[[float, Path | None, Path | None], float],
) -> dict[tuple[FragmentId, FragmentId], float]:
    model = _get_embed_model()
    if model is None or len(fragments) < 2:
        return {}

    texts = [f.content[:_EMBED_MAX_CHARS] for f in fragments]
    embeddings = model.encode(texts, normalize_embeddings=True, show_progress_bar=False)
    sim_matrix = embeddings @ embeddings.T

    edges: dict[tuple[FragmentId, FragmentId], float] = {}
    for i, f1 in enumerate(fragments):
        scores = [(float(sim_matrix[i, j]), j) for j in range(len(fragments)) if i != j]
        for score, j in sorted(scores, reverse=True)[:_TOP_K_NEIGHBORS]:
            if score < _MIN_SIMILARITY:
                break
            f2 = fragments[j]
            weight = clamp_weight_fn(score, f1.path, f2.path) * _EMBED_WEIGHT
            edges[(f1.id, f2.id)] = weight
            edges[(f2.id, f1.id)] = weight * _BACKWARD_WEIGHT_FACTOR

    return edges
