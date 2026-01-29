from __future__ import annotations

from collections import defaultdict
from pathlib import Path

from ...config import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict


class ContainmentEdgeBuilder(EdgeBuilder):
    weight = EDGE_WEIGHTS["containment"].forward
    reverse_weight_factor = EDGE_WEIGHTS["containment"].reverse_factor

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        by_path: dict[Path, list[Fragment]] = defaultdict(list)
        for f in fragments:
            by_path[f.path].append(f)

        edges: EdgeDict = {}

        for _path, frags in by_path.items():
            if len(frags) >= 2:
                self._process_file_containment(frags, edges)

        return edges

    def _process_file_containment(self, frags: list[Fragment], edges: EdgeDict) -> None:
        frags_sorted = sorted(frags, key=lambda x: (x.start_line, -x.end_line))
        stack: list[Fragment] = []

        for f in frags_sorted:
            while stack and f.start_line > stack[-1].end_line:
                stack.pop()

            if stack:
                self._add_containment_edge(f, stack[-1], edges)

            stack.append(f)

    def _add_containment_edge(self, child: Fragment, parent: Fragment, edges: EdgeDict) -> None:
        if parent.start_line <= child.start_line and child.end_line <= parent.end_line and parent.id != child.id:
            self.add_edge(edges, child.id, parent.id)


def _build_containment_edges(fragments: list[Fragment]) -> dict[tuple[FragmentId, FragmentId], float]:
    return ContainmentEdgeBuilder().build(fragments)
