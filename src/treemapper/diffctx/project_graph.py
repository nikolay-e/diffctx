from __future__ import annotations

import logging
from collections import defaultdict
from collections.abc import Iterator
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from ..ignore import get_ignore_specs, get_whitelist_spec
from ..tokens import count_tokens
from .config import LIMITS
from .fragmentation import _process_files_for_fragments
from .graph import Graph, build_graph
from .types import Fragment, FragmentId

logger = logging.getLogger(__name__)


@dataclass
class ProjectGraph:
    graph: Graph
    fragments: dict[FragmentId, Fragment] = field(default_factory=dict)
    files: dict[Path, list[FragmentId]] = field(default_factory=dict)
    root_dir: Path | None = None

    @property
    def node_count(self) -> int:
        return len(self.graph.nodes)

    @property
    def edge_count(self) -> int:
        return sum(len(nbrs) for nbrs in self.graph.adjacency.values())

    def edges_of_type(self, category: str) -> Iterator[tuple[FragmentId, FragmentId, float]]:
        for (src, dst), cat in self.graph.edge_categories.items():
            if cat == category:
                weight = self.graph.adjacency.get(src, {}).get(dst, 0.0)
                yield src, dst, weight

    def neighbors(self, fid: FragmentId) -> dict[FragmentId, float]:
        return self.graph.neighbors(fid)

    def subgraph(self, nodes: set[FragmentId]) -> ProjectGraph:
        sub = Graph()
        for n in nodes:
            sub.add_node(n)
        for src, nbrs in self.graph.adjacency.items():
            if src not in nodes:
                continue
            for dst, weight in nbrs.items():
                if dst in nodes:
                    sub.add_edge(src, dst, weight)
                    cat = self.graph.edge_categories.get((src, dst))
                    if cat:
                        sub.edge_categories[(src, dst)] = cat
        sub_frags = {fid: f for fid, f in self.fragments.items() if fid in nodes}
        sub_files: dict[Path, list[FragmentId]] = defaultdict(list)
        for fid in nodes:
            if fid in self.fragments:
                sub_files[fid.path].append(fid)
        return ProjectGraph(
            graph=sub,
            fragments=sub_frags,
            files=dict(sub_files),
            root_dir=self.root_dir,
        )

    def edge_type_counts(self) -> dict[str, int]:
        counts: dict[str, int] = defaultdict(int)
        for cat in self.graph.edge_categories.values():
            counts[cat] += 1
        return dict(counts)

    def top_by_in_degree(self, n: int = 10) -> list[tuple[FragmentId, int]]:
        in_deg: dict[FragmentId, int] = defaultdict(int)
        for nbrs in self.graph.reverse_adjacency.values():
            for fid in nbrs:
                in_deg[fid] += 1
        return sorted(in_deg.items(), key=lambda x: x[1], reverse=True)[:n]

    def to_dict(self) -> dict[str, Any]:
        root_name = self.root_dir.name if self.root_dir else "unknown"
        nodes = []
        for fid, frag in sorted(self.fragments.items(), key=lambda x: (x[0].path, x[0].start_line)):
            rel_path = _relative_path(fid.path, self.root_dir) if self.root_dir else str(fid.path)
            basename = fid.path.name
            loc = f"{basename}:{fid.start_line}-{fid.end_line}"
            label = f"{frag.symbol_name} ({loc})" if frag.symbol_name else loc
            nodes.append(
                {
                    "id": f"{rel_path}:{fid.start_line}-{fid.end_line}",
                    "label": label,
                    "path": rel_path,
                    "lines": f"{fid.start_line}-{fid.end_line}",
                    "kind": frag.kind,
                    "symbol": frag.symbol_name or "",
                    "token_count": frag.token_count,
                }
            )

        edges = []
        for (src, dst), cat in sorted(self.graph.edge_categories.items(), key=lambda x: str(x[0])):
            weight = self.graph.adjacency.get(src, {}).get(dst, 0.0)
            src_path = _relative_path(src.path, self.root_dir) if self.root_dir else str(src.path)
            dst_path = _relative_path(dst.path, self.root_dir) if self.root_dir else str(dst.path)
            src_frag = self.fragments.get(src)
            dst_frag = self.fragments.get(dst)
            edges.append(
                {
                    "source": f"{src_path}:{src.start_line}-{src.end_line}",
                    "source_symbol": (src_frag.symbol_name or "") if src_frag else "",
                    "target": f"{dst_path}:{dst.start_line}-{dst.end_line}",
                    "target_symbol": (dst_frag.symbol_name or "") if dst_frag else "",
                    "weight": round(weight, 4),
                    "category": cat,
                }
            )

        return {
            "name": root_name,
            "type": "project_graph",
            "node_count": self.node_count,
            "edge_count": self.edge_count,
            "nodes": nodes,
            "edges": edges,
        }


def _relative_path(path: Path, root: Path | None) -> str:
    if root is None:
        return str(path)
    try:
        return path.relative_to(root).as_posix()
    except ValueError:
        return str(path)


def build_project_graph(
    root_dir: Path,
    *,
    ignore_file: Path | None = None,
    no_default_ignores: bool = False,
    whitelist_file: Path | None = None,
) -> ProjectGraph:
    from .universe import _collect_candidate_files, _filter_whitelist

    root_dir = root_dir.resolve()
    combined_spec = get_ignore_specs(root_dir, ignore_file, no_default_ignores, None)
    wl_spec = get_whitelist_spec(whitelist_file, root_dir)

    all_candidate_files = _collect_candidate_files(root_dir, set(), combined_spec)
    all_candidate_files = _filter_whitelist(all_candidate_files, root_dir, wl_spec)

    logger.info("project_graph: found %d candidate files", len(all_candidate_files))

    seen_frag_ids: set[FragmentId] = set()
    all_fragments = _process_files_for_fragments(
        all_candidate_files,
        root_dir,
        preferred_revs=[],
        seen_frag_ids=seen_frag_ids,
        batch_reader=None,
    )

    overhead = LIMITS.overhead_per_fragment
    for frag in all_fragments:
        frag.token_count = count_tokens(frag.content).count + overhead

    logger.info("project_graph: %d fragments from %d files", len(all_fragments), len(all_candidate_files))

    graph = build_graph(all_fragments, repo_root=root_dir)

    frag_dict = {f.id: f for f in all_fragments}
    files_dict: dict[Path, list[FragmentId]] = defaultdict(list)
    for f in all_fragments:
        files_dict[f.path].append(f.id)

    return ProjectGraph(
        graph=graph,
        fragments=frag_dict,
        files=dict(files_dict),
        root_dir=root_dir,
    )
