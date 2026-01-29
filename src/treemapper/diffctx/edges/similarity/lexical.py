from __future__ import annotations

import math
from collections import defaultdict
from heapq import nlargest
from pathlib import Path

from ...config import (
    DEFAULT_LANG_WEIGHTS,
    LANG_WEIGHTS,
    LEXICAL,
    LangWeights,
)
from ...languages import EXTENSION_TO_LANGUAGE
from ...stopwords import TokenProfile, filter_idents
from ...types import Fragment, FragmentId, extract_identifier_list
from ..base import EdgeBuilder, EdgeDict


def _get_lang_weights(path: Path) -> LangWeights:
    suffix = path.suffix.lower()
    lang = EXTENSION_TO_LANGUAGE.get(suffix)
    return LANG_WEIGHTS.get(lang, DEFAULT_LANG_WEIGHTS) if lang else DEFAULT_LANG_WEIGHTS


def _clamp_lexical_weight(raw_sim: float, src_path: Path | None = None, dst_path: Path | None = None) -> float:
    if src_path and dst_path:
        src_weights = _get_lang_weights(src_path)
        dst_weights = _get_lang_weights(dst_path)
        lex_max = max(src_weights.lexical_max, dst_weights.lexical_max)
        lex_min = max(src_weights.lexical_min, dst_weights.lexical_min)
    else:
        lex_max = LEXICAL.weight_max
        lex_min = LEXICAL.weight_min

    if raw_sim < LEXICAL.min_similarity:
        return 0.0
    normalized = (raw_sim - LEXICAL.min_similarity) / (1.0 - LEXICAL.min_similarity)
    return lex_min + normalized * (lex_max - lex_min)


class LexicalEdgeBuilder(EdgeBuilder):
    weight = LEXICAL.weight_max
    reverse_weight_factor = LEXICAL.backward_factor

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        if not fragments:
            return {}

        doc_freq = self._compute_doc_frequencies(fragments)
        n_docs = len(fragments)
        max_df = max(1, int(n_docs * LEXICAL.max_df_ratio))
        idf = self._compute_idf_scores(doc_freq, n_docs)

        tf_idf_vectors = {frag.id: self._build_tf_idf_vector(frag, doc_freq, idf, max_df) for frag in fragments}
        postings = self._build_postings_index(tf_idf_vectors)
        dot_products = self._compute_dot_products(postings)

        id_to_path = {frag.id: frag.path for frag in fragments}
        neighbors_by_node = self._collect_neighbors(dot_products, id_to_path)

        edges: EdgeDict = {}
        for node, candidates in neighbors_by_node.items():
            top_k = nlargest(LEXICAL.top_k_neighbors, candidates, key=lambda x: x[0])
            for weight, neighbor in top_k:
                edges[(node, neighbor)] = weight

        return edges

    def _compute_doc_frequencies(self, fragments: list[Fragment]) -> dict[str, int]:
        doc_freq: dict[str, int] = defaultdict(int)
        for frag in fragments:
            seen_in_doc: set[str] = set()
            profile = TokenProfile.from_path(str(frag.path))
            idents = filter_idents(extract_identifier_list(frag.content, profile=profile), min_len=3, profile=profile)
            for ident in idents:
                if ident not in seen_in_doc:
                    doc_freq[ident] += 1
                    seen_in_doc.add(ident)
        return doc_freq

    def _compute_idf_scores(self, doc_freq: dict[str, int], n_docs: int) -> dict[str, float]:
        return {term: math.log((n_docs + 1) / (df + 1)) + 1 for term, df in doc_freq.items()}

    def _build_tf_idf_vector(
        self, frag: Fragment, doc_freq: dict[str, int], idf: dict[str, float], max_df: int
    ) -> dict[str, float]:
        tf: dict[str, int] = defaultdict(int)
        profile = TokenProfile.from_path(str(frag.path))
        idents = filter_idents(extract_identifier_list(frag.content, profile=profile), min_len=3, profile=profile)
        for ident in idents:
            tf[ident] += 1

        vec: dict[str, float] = {}
        for term, count in tf.items():
            df = doc_freq.get(term, 0)
            if df <= 0 or df > max_df:
                continue
            term_idf = idf.get(term, 1.0)
            if term_idf < LEXICAL.min_idf:
                continue
            vec[term] = count * term_idf

        norm = math.sqrt(sum(v * v for v in vec.values())) if vec else 0.0
        if norm > 0:
            for term in vec:
                vec[term] /= norm

        return vec

    def _build_postings_index(
        self, tf_idf_vectors: dict[FragmentId, dict[str, float]]
    ) -> dict[str, list[tuple[FragmentId, float]]]:
        postings: dict[str, list[tuple[FragmentId, float]]] = defaultdict(list)
        for frag_id, vec in tf_idf_vectors.items():
            for term, weight in vec.items():
                postings[term].append((frag_id, weight))
        return postings

    def _compute_dot_products(
        self, postings: dict[str, list[tuple[FragmentId, float]]]
    ) -> dict[tuple[FragmentId, FragmentId], float]:
        dot_products: dict[tuple[FragmentId, FragmentId], float] = defaultdict(float)

        for term, posting_list in postings.items():
            if len(posting_list) > LEXICAL.max_postings:
                continue
            for i, (frag_i, weight_i) in enumerate(posting_list):
                for frag_j, weight_j in posting_list[i + 1 :]:
                    pair = (frag_i, frag_j) if str(frag_i) < str(frag_j) else (frag_j, frag_i)
                    dot_products[pair] += weight_i * weight_j

        return dot_products

    def _collect_neighbors(
        self, dot_products: dict[tuple[FragmentId, FragmentId], float], id_to_path: dict[FragmentId, Path]
    ) -> dict[FragmentId, list[tuple[float, FragmentId]]]:
        neighbors_by_node: dict[FragmentId, list[tuple[float, FragmentId]]] = defaultdict(list)

        for (src, dst), sim in dot_products.items():
            if sim < LEXICAL.min_similarity:
                continue
            src_path = id_to_path.get(src)
            dst_path = id_to_path.get(dst)
            clamped_forward = _clamp_lexical_weight(sim, src_path, dst_path)
            clamped_backward = _clamp_lexical_weight(sim, dst_path, src_path) * LEXICAL.backward_factor
            neighbors_by_node[src].append((clamped_forward, dst))
            neighbors_by_node[dst].append((clamped_backward, src))

        return neighbors_by_node


def _build_lexical_edges_sparse(fragments: list[Fragment]) -> dict[tuple[FragmentId, FragmentId], float]:
    return LexicalEdgeBuilder().build(fragments)


def clamp_lexical_weight(raw_sim: float, src_path: Path | None = None, dst_path: Path | None = None) -> float:
    return _clamp_lexical_weight(raw_sim, src_path, dst_path)
