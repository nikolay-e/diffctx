from __future__ import annotations

from abc import ABC, abstractmethod
from collections import defaultdict
from pathlib import Path

from ..types import Fragment, FragmentId

EdgeDict = dict[tuple[FragmentId, FragmentId], float]


def path_to_module(path: Path, repo_root: Path | None = None) -> str:
    if repo_root and path.is_absolute():
        try:
            path = path.relative_to(repo_root)
        except ValueError:
            pass

    parts = list(path.parts)

    for i, part in enumerate(parts):
        if part in ("src", "lib", "packages"):
            parts = parts[i + 1 :]
            break

    if parts and parts[-1].endswith(".py"):
        parts[-1] = parts[-1][:-3]
        if parts[-1] == "__init__":
            parts = parts[:-1]

    return ".".join(parts) if parts else ""


def build_path_to_frags(fragments: list[Fragment], repo_root: Path | None = None) -> dict[Path, list[FragmentId]]:
    path_to_frags: dict[Path, list[FragmentId]] = defaultdict(list)
    for f in fragments:
        path_to_frags[f.path].append(f.id)
        if repo_root:
            try:
                rel = f.path.relative_to(repo_root)
                path_to_frags[rel].append(f.id)
            except ValueError:
                pass
    return path_to_frags


class FragmentIndex:
    def __init__(self, fragments: list[Fragment], repo_root: Path | None = None):
        self.by_name: dict[str, list[FragmentId]] = defaultdict(list)
        self.by_path: dict[str, list[FragmentId]] = defaultdict(list)

        for f in fragments:
            self.by_name[f.path.name.lower()].append(f.id)
            self.by_path[str(f.path)].append(f.id)

            if repo_root:
                try:
                    rel = f.path.relative_to(repo_root)
                    self.by_path[str(rel)].append(f.id)
                    self.by_path[rel.as_posix()].append(f.id)
                except ValueError:
                    pass


def discover_files_by_refs(
    refs: set[str],
    changed_files: list[Path],
    all_candidate_files: list[Path],
) -> list[Path]:
    if not refs:
        return []

    discovered: list[Path] = []
    changed_set = set(changed_files)

    for candidate in all_candidate_files:
        if candidate in changed_set:
            continue
        candidate_name = candidate.name.lower()
        candidate_str = str(candidate).lower()

        for ref in refs:
            ref_lower = ref.lower()
            ref_name = ref.split("/")[-1].lower()
            if candidate_name == ref_name or ref_lower in candidate_str:
                discovered.append(candidate)
                break

    return discovered


def add_ref_edges(
    edges: EdgeDict,
    src_id: FragmentId,
    names: set[str],
    name_to_defs: dict[str, list[FragmentId]],
    weight: float,
    reverse_factor: float = 0.7,
    skip_self_defs: set[str] | None = None,
) -> None:
    for name in names:
        if skip_self_defs and name in skip_self_defs:
            continue
        for dst in name_to_defs.get(name, []):
            if dst == src_id:
                continue
            edges[(src_id, dst)] = max(edges.get((src_id, dst), 0.0), weight)
            edges[(dst, src_id)] = max(edges.get((dst, src_id), 0.0), weight * reverse_factor)


class EdgeBuilder(ABC):
    weight: float = 0.5
    reverse_weight_factor: float = 0.7

    @abstractmethod
    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        pass

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        return []

    def add_edge(
        self,
        edges: EdgeDict,
        src: FragmentId,
        dst: FragmentId,
        weight: float | None = None,
        bidirectional: bool = True,
    ) -> None:
        w = weight if weight is not None else self.weight
        edges[(src, dst)] = max(edges.get((src, dst), 0.0), w)
        if bidirectional:
            reverse = w * self.reverse_weight_factor
            edges[(dst, src)] = max(edges.get((dst, src), 0.0), reverse)

    def add_edge_no_reverse(
        self,
        edges: EdgeDict,
        src: FragmentId,
        dst: FragmentId,
        weight: float | None = None,
    ) -> None:
        w = weight if weight is not None else self.weight
        edges[(src, dst)] = max(edges.get((src, dst), 0.0), w)

    def add_edges_from_ids(
        self,
        source_id: FragmentId,
        target_ids: list[FragmentId],
        weight: float,
        edges: EdgeDict,
    ) -> None:
        for fid in target_ids:
            if fid != source_id:
                self.add_edge(edges, source_id, fid, weight)

    def link_by_path_match(
        self,
        src_id: FragmentId,
        ref: str,
        idx: FragmentIndex,
        edges: EdgeDict,
        weight: float | None = None,
    ) -> None:
        w = weight if weight is not None else self.weight
        ref_lower = ref.lower()
        for path_str, frag_ids in idx.by_path.items():
            if ref in path_str or ref_lower in path_str.lower():
                for fid in frag_ids:
                    if fid != src_id:
                        self.add_edge(edges, src_id, fid, w)

    def link_by_name_match(
        self,
        src_id: FragmentId,
        ref_name: str,
        idx: FragmentIndex,
        edges: EdgeDict,
        weight: float | None = None,
    ) -> None:
        w = weight if weight is not None else self.weight
        ref_lower = ref_name.lower()
        for name, frag_ids in idx.by_name.items():
            if name == ref_lower:
                for fid in frag_ids:
                    if fid != src_id:
                        self.add_edge(edges, src_id, fid, w)

    @staticmethod
    def index_paths_for_fragment(
        f: Fragment,
        path_index: dict[str, list[FragmentId]],
        repo_root: Path | None,
    ) -> None:
        if not repo_root:
            return
        try:
            rel = f.path.relative_to(repo_root)
            path_index[str(rel)].append(f.id)
            path_index[rel.as_posix()].append(f.id)
        except ValueError:
            pass
