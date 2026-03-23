from __future__ import annotations

import re
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_BAZEL_NAMES = frozenset(
    {
        "BUILD",
        "BUILD.bazel",
        "WORKSPACE",
        "WORKSPACE.bazel",
    }
)
_BAZEL_EXTS = {".bzl", ".bazel"}

_DEPS_RE = re.compile(
    r"""["']([/@][^"']{1,300})["']""",
)
_LOAD_RE = re.compile(
    r"""load\(\s*["']([^"']{1,300})["']\s*,""",
)
_SRCS_RE = re.compile(
    r"""["']([^"']{1,300}\.\w{1,10})["']""",
)
_LABEL_RE = re.compile(
    r"//([^:\"']{1,200}):([^\"'\s,\]]{1,200})",
)


def _is_bazel_file(path: Path) -> bool:
    if path.name in _BAZEL_NAMES:
        return True
    return path.suffix.lower() in _BAZEL_EXTS


def _extract_labels(content: str) -> set[str]:
    labels: set[str] = set()
    for m in _DEPS_RE.finditer(content):
        labels.add(m.group(1))
    return labels


def _extract_loads(content: str) -> set[str]:
    loads: set[str] = set()
    for m in _LOAD_RE.finditer(content):
        loads.add(m.group(1))
    return loads


def _extract_srcs(content: str) -> set[str]:
    srcs: set[str] = set()
    in_srcs = False
    for line in content.splitlines():
        stripped = line.strip()
        if "srcs" in stripped and "=" in stripped:
            in_srcs = True
        if in_srcs:
            for m in _SRCS_RE.finditer(line):
                srcs.add(m.group(1))
            if "]" in stripped:
                in_srcs = False
    return srcs


def _label_to_path(label: str) -> str | None:
    m = _LABEL_RE.search(label)
    if m:
        return m.group(1)
    if label.startswith("//"):
        cleaned = label.lstrip("/").split(":")[0]
        if cleaned:
            return cleaned
    return None


def _ref_to_filename(ref: str) -> str:
    name = ref.rstrip("/").split("/")[-1].split(":")[-1].lower()
    return name


class BazelEdgeBuilder(EdgeBuilder):
    weight = 0.65
    deps_weight = EDGE_WEIGHTS["bazel_deps"].forward
    load_weight = EDGE_WEIGHTS["bazel_load"].forward
    srcs_weight = EDGE_WEIGHTS["bazel_srcs"].forward
    reverse_weight_factor = EDGE_WEIGHTS["bazel_deps"].reverse_factor

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        bazel_changed = [f for f in changed_files if _is_bazel_file(f)]
        if not bazel_changed:
            return []

        refs: set[str] = set()
        for f in bazel_changed:
            self._collect_refs_from_file(f, refs)

        self._discover_reverse_load_refs(bazel_changed, all_candidate_files, refs)

        return discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

    def _collect_refs_from_file(self, f: Path, refs: set[str]) -> None:
        try:
            content = f.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            return
        for label in _extract_labels(content):
            path = _label_to_path(label)
            if path:
                refs.add(path)
                refs.add(f"{path}/BUILD")
                refs.add(f"{path}/BUILD.bazel")
        for load in _extract_loads(content):
            path = _label_to_path(load)
            if path:
                refs.add(path)
            refs.add(_ref_to_filename(load))
        for src in _extract_srcs(content):
            refs.add(src)
            refs.add(str(f.parent / src))

    def _discover_reverse_load_refs(
        self,
        bazel_changed: list[Path],
        all_candidate_files: list[Path],
        refs: set[str],
    ) -> None:
        changed_names = {f.name.lower() for f in bazel_changed}
        changed_paths = self._build_changed_paths(bazel_changed)

        for candidate in all_candidate_files:
            if not _is_bazel_file(candidate):
                continue
            self._check_candidate_loads(candidate, changed_names, changed_paths, refs)

    @staticmethod
    def _build_changed_paths(bazel_changed: list[Path]) -> set[str]:
        changed_paths: set[str] = set()
        for f in bazel_changed:
            changed_paths.add(str(f))
            if f.suffix == ".bzl":
                changed_paths.add(f.stem)
        return changed_paths

    @staticmethod
    def _check_candidate_loads(
        candidate: Path,
        changed_names: set[str],
        changed_paths: set[str],
        refs: set[str],
    ) -> None:
        try:
            content = candidate.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            return
        for load in _extract_loads(content):
            load_file = _ref_to_filename(load)
            if load_file in changed_names or any(cp in load for cp in changed_paths):
                refs.add(candidate.name.lower())

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        bazel_frags = [f for f in fragments if _is_bazel_file(f.path)]
        if not bazel_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)

        for bf in bazel_frags:
            self._add_fragment_edges(bf, idx, edges, fragments)

        return edges

    def _add_fragment_edges(
        self,
        bf: Fragment,
        idx: FragmentIndex,
        edges: EdgeDict,
        all_fragments: list[Fragment],
    ) -> None:
        for label in _extract_labels(bf.content):
            path = _label_to_path(label)
            if path:
                self._link_label_path(bf.id, path, idx, edges, self.deps_weight)

        for load in _extract_loads(bf.content):
            self._link_load(bf.id, load, idx, edges)

        for src in _extract_srcs(bf.content):
            self._link_src(bf.id, src, bf.path, idx, edges, all_fragments)

    def _link_label_path(
        self,
        src_id: FragmentId,
        path: str,
        idx: FragmentIndex,
        edges: EdgeDict,
        weight: float,
    ) -> None:
        for build_name in ("BUILD", "BUILD.bazel"):
            build_path = f"{path}/{build_name}"
            self.link_by_path_match(src_id, build_path, idx, edges, weight)

        self.link_by_path_match(src_id, path, idx, edges, weight)

    def _link_load(
        self,
        src_id: FragmentId,
        load: str,
        idx: FragmentIndex,
        edges: EdgeDict,
    ) -> None:
        filename = _ref_to_filename(load)
        for name, frag_ids in idx.by_name.items():
            if name == filename or name == filename.replace(".bzl", ""):
                for fid in frag_ids:
                    if fid != src_id:
                        self.add_edge(edges, src_id, fid, self.load_weight)
                        return

        path = _label_to_path(load)
        if path:
            self.link_by_path_match(src_id, path, idx, edges, self.load_weight)

    def _link_src(
        self,
        src_id: FragmentId,
        src: str,
        build_path: Path,
        idx: FragmentIndex,
        edges: EdgeDict,
        all_fragments: list[Fragment],
    ) -> None:
        src_lower = src.lower()
        if self._try_link_src_by_name(src_id, src_lower, build_path, idx, edges, all_fragments):
            return
        rel_path = str(build_path.parent / src)
        self.link_by_path_match(src_id, rel_path, idx, edges, self.srcs_weight)

    def _try_link_src_by_name(
        self,
        src_id: FragmentId,
        src_lower: str,
        build_path: Path,
        idx: FragmentIndex,
        edges: EdgeDict,
        all_fragments: list[Fragment],
    ) -> bool:
        frag_ids = idx.by_name.get(src_lower)
        if not frag_ids:
            return False
        for fid in frag_ids:
            if fid == src_id:
                continue
            if self._is_sibling_fragment(fid, build_path, all_fragments):
                self.add_edge(edges, src_id, fid, self.srcs_weight)
                return True
        return False

    @staticmethod
    def _is_sibling_fragment(fid: FragmentId, build_path: Path, all_fragments: list[Fragment]) -> bool:
        return any(frag.id == fid and frag.path.parent == build_path.parent for frag in all_fragments)
