from __future__ import annotations

from collections import defaultdict
from pathlib import Path

from ...config import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict


class DocumentStructureEdgeBuilder(EdgeBuilder):
    weight = EDGE_WEIGHTS["doc_structure"].forward
    reverse_weight_factor = EDGE_WEIGHTS["doc_structure"].reverse_factor

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        edges: EdgeDict = {}

        by_path: dict[Path, list[Fragment]] = defaultdict(list)
        for f in fragments:
            if f.kind in ("section", "paragraph"):
                by_path[f.path].append(f)

        for _path, frags in by_path.items():
            frags_sorted = sorted(frags, key=lambda x: x.start_line)

            for i in range(len(frags_sorted) - 1):
                curr, next_f = frags_sorted[i], frags_sorted[i + 1]
                self.add_edge(edges, curr.id, next_f.id)

        return edges


def _build_document_structure_edges(fragments: list[Fragment]) -> dict[tuple[FragmentId, FragmentId], float]:
    return DocumentStructureEdgeBuilder().build(fragments)
