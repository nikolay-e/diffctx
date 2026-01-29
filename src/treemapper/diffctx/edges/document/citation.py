from __future__ import annotations

from collections import defaultdict
from pathlib import Path

from ...config import DOC_PATTERNS, EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict


class CitationEdgeBuilder(EdgeBuilder):
    weight = EDGE_WEIGHTS["citation"].forward
    reverse_weight_factor = EDGE_WEIGHTS["citation"].reverse_factor

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        citation_to_frags: dict[str, list[FragmentId]] = defaultdict(list)

        for f in fragments:
            for cit in DOC_PATTERNS["citation"].findall(f.content):
                citation_to_frags[cit].append(f.id)

        edges: EdgeDict = {}

        for _cit, frag_ids in citation_to_frags.items():
            if len(frag_ids) < 2:
                continue
            hub = frag_ids[0]
            for other in frag_ids[1:]:
                edges[(hub, other)] = self.weight
                edges[(other, hub)] = self.weight

        return edges


def _build_citation_edges_sparse(fragments: list[Fragment]) -> dict[tuple[FragmentId, FragmentId], float]:
    return CitationEdgeBuilder().build(fragments)
