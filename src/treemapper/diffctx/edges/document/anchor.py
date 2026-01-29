from __future__ import annotations

import re
from pathlib import Path

from ...config import DOC_PATTERNS, EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict


def _slugify(text: str) -> str:
    text = text.lower()
    text = re.sub(r"[^\w\s-]", "", text)
    text = re.sub(r"[\s_]+", "-", text)
    return text.strip("-")


class AnchorLinkEdgeBuilder(EdgeBuilder):
    weight = EDGE_WEIGHTS["anchor_link"].forward
    reverse_weight_factor = EDGE_WEIGHTS["anchor_link"].reverse_factor

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        anchor_index = self._build_anchor_index(fragments)
        edges: EdgeDict = {}

        for f in fragments:
            self._add_anchor_link_edges_for_fragment(f, anchor_index, edges)

        return edges

    def _build_anchor_index(self, fragments: list[Fragment]) -> dict[str, FragmentId]:
        anchor_index: dict[str, FragmentId] = {}
        for f in fragments:
            if f.kind == "section":
                first_line = f.content.split("\n")[0]
                heading = re.sub(r"^#+\s*", "", first_line).strip()
                slug = _slugify(heading)
                if slug:
                    anchor_index[slug] = f.id
        return anchor_index

    def _add_anchor_link_edges_for_fragment(self, f: Fragment, anchor_index: dict[str, FragmentId], edges: EdgeDict) -> None:
        for match in DOC_PATTERNS["md_internal_link"].finditer(f.content):
            target_slug = _slugify(match.group(2))
            target_id = anchor_index.get(target_slug)
            if target_id and target_id != f.id:
                self.add_edge(edges, f.id, target_id)


def _build_anchor_link_edges(fragments: list[Fragment]) -> dict[tuple[FragmentId, FragmentId], float]:
    return AnchorLinkEdgeBuilder().build(fragments)
