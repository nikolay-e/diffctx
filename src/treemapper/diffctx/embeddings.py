from __future__ import annotations

import logging
import os
import threading
from collections.abc import Callable
from pathlib import Path
from typing import TYPE_CHECKING

from .types import Fragment, FragmentId

if TYPE_CHECKING:
    from sentence_transformers import SentenceTransformer

_EMBED_WEIGHT = 0.35
_EMBED_MAX_CHARS = 2048
_EMBED_MODEL_NAME = "jinaai/jina-embeddings-v2-base-code"
_TRUST_REMOTE_CODE = os.getenv("TREEMAPPER_TRUST_REMOTE_CODE", "0") == "1"

_TOP_K_NEIGHBORS = 10
_MIN_SIMILARITY = 0.1
_BACKWARD_WEIGHT_FACTOR = 0.7

_EMBED_MODEL: SentenceTransformer | None = None
_EMBED_LOCK = threading.Lock()
_EMBED_AVAILABLE: bool | None = None

_BATCH_SIZE = 500


def _get_embed_model() -> SentenceTransformer | None:
    global _EMBED_MODEL, _EMBED_AVAILABLE
    if _EMBED_AVAILABLE is False:
        return None
    if _EMBED_MODEL is None:
        with _EMBED_LOCK:
            if _EMBED_MODEL is None:
                try:
                    from sentence_transformers import SentenceTransformer

                    _EMBED_MODEL = SentenceTransformer(_EMBED_MODEL_NAME, trust_remote_code=_TRUST_REMOTE_CODE)
                    _EMBED_AVAILABLE = True
                    logging.debug("diffctx: loaded embedding model %s", _EMBED_MODEL_NAME)
                except ImportError:
                    logging.debug("diffctx: sentence-transformers not installed, skipping embeddings")
                    _EMBED_AVAILABLE = False
                    return None
                except Exception as e:
                    logging.debug("diffctx: failed to load embedding model: %s", e)
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

    import numpy as np

    texts = [f.content[:_EMBED_MAX_CHARS] for f in fragments]
    embeddings = model.encode(texts, normalize_embeddings=True, show_progress_bar=False)

    edges: dict[tuple[FragmentId, FragmentId], float] = {}
    n = len(fragments)

    for i in range(0, n, _BATCH_SIZE):
        batch = embeddings[i : i + _BATCH_SIZE]
        sims = batch @ embeddings.T

        for j_local in range(len(batch)):
            j_global = i + j_local
            row = sims[j_local].copy()
            row[j_global] = -1.0

            top_k = min(_TOP_K_NEIGHBORS, n - 1)
            if top_k <= 0:
                continue
            top_indices = np.argpartition(row, -top_k)[-top_k:]

            f1 = fragments[j_global]
            for k_idx in top_indices:
                score = float(row[k_idx])
                if score < _MIN_SIMILARITY:
                    continue
                f2 = fragments[k_idx]
                weight = clamp_weight_fn(score, f1.path, f2.path) * _EMBED_WEIGHT
                edges[(f1.id, f2.id)] = max(edges.get((f1.id, f2.id), 0.0), weight)
                edges[(f2.id, f1.id)] = max(edges.get((f2.id, f1.id), 0.0), weight * _BACKWARD_WEIGHT_FACTOR)

    return edges
