from __future__ import annotations

import math
import subprocess
from collections import Counter, defaultdict
from pathlib import Path

from ...config import COCHANGE, EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict


class CochangeEdgeBuilder(EdgeBuilder):
    weight = EDGE_WEIGHTS["cochange"].forward
    reverse_weight_factor = EDGE_WEIGHTS["cochange"].reverse_factor

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        if repo_root is None:
            return {}

        commits = self._get_git_log_files(repo_root)
        if commits is None:
            return {}

        cochange = self._count_cochanges(commits)
        path_to_frags = self._build_path_to_frags_index(fragments, repo_root)

        edges: EdgeDict = {}
        for (p1, p2), count in cochange.items():
            if count < COCHANGE.min_count:
                continue
            weight = min(self.weight, 0.1 * math.log(1 + count))
            for fid1 in path_to_frags.get(p1, []):
                for fid2 in path_to_frags.get(p2, []):
                    edges[(fid1, fid2)] = max(edges.get((fid1, fid2), 0.0), weight)
                    edges[(fid2, fid1)] = max(edges.get((fid2, fid1), 0.0), weight)

        return edges

    def _get_git_log_files(self, repo_root: Path) -> list[list[str]] | None:
        try:
            result = subprocess.run(
                ["git", "-C", str(repo_root), "log", "--name-only", "--format=", f"-n{COCHANGE.commits_limit}"],
                capture_output=True,
                text=True,
                check=True,
                timeout=COCHANGE.timeout_seconds,
            )
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired, FileNotFoundError):
            return None
        return [c.strip().split("\n") for c in result.stdout.split("\n\n") if c.strip()]

    def _count_cochanges(self, commits: list[list[str]]) -> Counter[tuple[str, str]]:
        cochange: Counter[tuple[str, str]] = Counter()
        for files in commits:
            files = [f for f in files if f]
            if len(files) > COCHANGE.max_files_per_commit:
                continue
            for i, f1 in enumerate(files):
                for f2 in files[i + 1 :]:
                    pair = (f1, f2) if f1 < f2 else (f2, f1)
                    cochange[pair] += 1
        return cochange

    def _build_path_to_frags_index(self, fragments: list[Fragment], repo_root: Path) -> dict[str, list[FragmentId]]:
        path_to_frags: dict[str, list[FragmentId]] = defaultdict(list)
        for f in fragments:
            try:
                rel = f.path.relative_to(repo_root).as_posix() if f.path.is_absolute() else f.path.as_posix()
                path_to_frags[rel].append(f.id)
            except ValueError:
                continue
        return path_to_frags


def _build_cochange_edges(fragments: list[Fragment], repo_root: Path | None) -> dict[tuple[FragmentId, FragmentId], float]:
    return CochangeEdgeBuilder().build(fragments, repo_root)
