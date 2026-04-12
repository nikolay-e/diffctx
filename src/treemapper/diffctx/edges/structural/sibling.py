from __future__ import annotations

from collections import defaultdict
from pathlib import Path

from ...config import EDGE_WEIGHTS, SIBLING
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict


class SiblingEdgeBuilder(EdgeBuilder):
    weight = EDGE_WEIGHTS["sibling"].forward
    reverse_weight_factor = EDGE_WEIGHTS["sibling"].reverse_factor
    category = "sibling"

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        by_dir = self._group_files_by_dir(fragments)
        file_to_rep = self._build_file_representative_map(fragments)
        edges: EdgeDict = {}

        for _dir_path, files in by_dir.items():
            self._add_sibling_edges_for_dir(files, file_to_rep, edges)

        return edges

    def _group_files_by_dir(self, fragments: list[Fragment]) -> dict[Path, set[Path]]:
        by_dir: dict[Path, set[Path]] = defaultdict(set)
        for f in fragments:
            by_dir[f.path.parent].add(f.path)
        return by_dir

    def _build_file_representative_map(self, fragments: list[Fragment]) -> dict[Path, FragmentId]:
        frag_by_id: dict[FragmentId, Fragment] = {f.id: f for f in fragments}
        file_to_rep: dict[Path, FragmentId] = {}
        for f in fragments:
            if f.path not in file_to_rep:
                file_to_rep[f.path] = f.id
            elif f.token_count > 0:
                existing = frag_by_id.get(file_to_rep[f.path])
                if existing and f.token_count > existing.token_count:
                    file_to_rep[f.path] = f.id
        return file_to_rep

    def _add_sibling_edges_for_dir(self, files: set[Path], file_to_rep: dict[Path, FragmentId], edges: EdgeDict) -> None:
        file_list = sorted(files)
        if len(file_list) > SIBLING.max_files_per_dir:
            file_list = file_list[: SIBLING.max_files_per_dir]

        if len(file_list) < 2:
            return

        for i, f1_path in enumerate(file_list):
            for f2_path in file_list[i + 1 :]:
                f1_id = file_to_rep.get(f1_path)
                f2_id = file_to_rep.get(f2_path)
                if f1_id and f2_id:
                    self.add_edge(edges, f1_id, f2_id, self.weight)
