from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, FragmentIndex

_WORKSPACE_MEMBERS_RE = re.compile(
    r"\[workspace\][^\[]*?members\s*=\s*\[(.*?)\]",
    re.DOTALL,
)
_PATH_DEP_RE = re.compile(
    r"""^\s*(\w[\w-]{0,100})\s*=\s*\{[^}]*?path\s*=\s*["']([^"']{1,300})["']""",
    re.MULTILINE,
)
_FEATURES_SECTION_RE = re.compile(
    r"^\[features\]\s*\n((?:(?!\[)[^\n]*\n)*)",
    re.MULTILINE,
)
_FEATURE_DEP_RE = re.compile(r"""["'](\w[\w-]{0,100})(?:/[^"']*)?["']""")
_STRING_ITEM_RE = re.compile(r"""["']([^"']{1,300})["']""")

_BIN_SECTION_RE = re.compile(
    r"""\[\[bin\]\][^\[]*?path\s*=\s*["']([^"']{1,300})["']""",
    re.DOTALL,
)
_LIB_SECTION_RE = re.compile(
    r"""\[lib\][^\[]*?path\s*=\s*["']([^"']{1,300})["']""",
    re.DOTALL,
)


def _is_cargo_toml(path: Path) -> bool:
    return path.name.lower() == "cargo.toml"


def _is_rust_source(path: Path) -> bool:
    return path.suffix.lower() == ".rs"


def _extract_workspace_members(content: str) -> list[str]:
    m = _WORKSPACE_MEMBERS_RE.search(content)
    if not m:
        return []
    return _STRING_ITEM_RE.findall(m.group(1))


def _extract_path_deps(content: str) -> list[tuple[str, str]]:
    return [(m.group(1), m.group(2)) for m in _PATH_DEP_RE.finditer(content)]


def _extract_feature_deps(content: str) -> set[str]:
    deps: set[str] = set()
    m = _FEATURES_SECTION_RE.search(content)
    if not m:
        return deps
    for fm in _FEATURE_DEP_RE.finditer(m.group(1)):
        deps.add(fm.group(1))
    return deps


def _extract_entry_points(content: str) -> list[str]:
    entries: list[str] = []
    for m in _BIN_SECTION_RE.finditer(content):
        entries.append(m.group(1))
    lib_match = _LIB_SECTION_RE.search(content)
    if lib_match:
        entries.append(lib_match.group(1))
    return entries


class CargoEdgeBuilder(EdgeBuilder):
    weight = 0.65
    workspace_weight = EDGE_WEIGHTS["cargo_workspace"].forward
    path_dep_weight = EDGE_WEIGHTS["cargo_path_dep"].forward
    entry_point_weight = EDGE_WEIGHTS["cargo_entry_point"].forward
    reverse_weight_factor = EDGE_WEIGHTS["cargo_workspace"].reverse_factor

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        cargo_changed = [f for f in changed_files if _is_cargo_toml(f)]
        if not cargo_changed:
            return []

        changed_set = set(changed_files)
        discovered: list[Path] = []
        for cargo_file in cargo_changed:
            self._discover_from_cargo_file(cargo_file, changed_set, discovered)

        valid = set(all_candidate_files)
        return [d for d in discovered if d in valid]

    def _discover_from_cargo_file(
        self,
        cargo_file: Path,
        changed_set: set[Path],
        discovered: list[Path],
    ) -> None:
        try:
            content = cargo_file.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            return

        parent = cargo_file.parent

        for entry in _get_default_entry_points(content):
            candidate = parent / entry
            if candidate not in changed_set:
                discovered.append(candidate)

        for _, rel_path in _extract_path_deps(content):
            dep_cargo = (parent / rel_path / "Cargo.toml").resolve()
            if dep_cargo not in changed_set:
                discovered.append(dep_cargo)

        for member in _extract_workspace_members(content):
            member_cargo = (parent / member / "Cargo.toml").resolve()
            if member_cargo not in changed_set:
                discovered.append(member_cargo)

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        cargo_frags = [f for f in fragments if _is_cargo_toml(f.path)]
        if not cargo_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)
        cargo_by_dir = self._build_cargo_dir_index(cargo_frags)
        rs_by_path = self._build_rs_path_index(fragments)

        for cf in cargo_frags:
            self._link_entry_points(cf, rs_by_path, edges)
            self._link_path_deps(cf, cargo_by_dir, idx, edges)
            self._link_workspace_members(cf, cargo_by_dir, idx, edges)
            self._link_feature_deps(cf, cargo_by_dir, idx, edges)

        return edges

    def _build_cargo_dir_index(self, cargo_frags: list[Fragment]) -> dict[str, list[FragmentId]]:
        cargo_by_dir: dict[str, list[FragmentId]] = defaultdict(list)
        for f in cargo_frags:
            cargo_by_dir[str(f.path.parent)].append(f.id)
        return cargo_by_dir

    def _build_rs_path_index(self, fragments: list[Fragment]) -> dict[str, FragmentId]:
        rs_by_path: dict[str, FragmentId] = {}
        for f in fragments:
            if _is_rust_source(f.path):
                rs_by_path[str(f.path)] = f.id
        return rs_by_path

    def _link_entry_points(
        self,
        cf: Fragment,
        rs_by_path: dict[str, FragmentId],
        edges: EdgeDict,
    ) -> None:
        parent = cf.path.parent
        for entry in _get_default_entry_points(cf.content):
            entry_path = str(parent / entry)
            fid = rs_by_path.get(entry_path)
            if fid and fid != cf.id:
                self.add_edge(edges, cf.id, fid, self.entry_point_weight)

    def _link_path_deps(
        self,
        cf: Fragment,
        cargo_by_dir: dict[str, list[FragmentId]],
        _idx: FragmentIndex,
        edges: EdgeDict,
    ) -> None:
        parent = cf.path.parent
        for _, rel_path in _extract_path_deps(cf.content):
            try:
                dep_dir = str((parent / rel_path).resolve())
            except (OSError, ValueError):
                continue
            for fid in cargo_by_dir.get(dep_dir, []):
                if fid != cf.id:
                    self.add_edge(edges, cf.id, fid, self.path_dep_weight)

    def _link_workspace_members(
        self,
        cf: Fragment,
        cargo_by_dir: dict[str, list[FragmentId]],
        _idx: FragmentIndex,
        edges: EdgeDict,
    ) -> None:
        parent = cf.path.parent
        for member in _extract_workspace_members(cf.content):
            try:
                member_dir = str((parent / member).resolve())
            except (OSError, ValueError):
                continue
            for fid in cargo_by_dir.get(member_dir, []):
                if fid != cf.id:
                    self.add_edge(edges, cf.id, fid, self.workspace_weight)

    def _link_feature_deps(
        self,
        cf: Fragment,
        cargo_by_dir: dict[str, list[FragmentId]],
        _idx: FragmentIndex,
        edges: EdgeDict,
    ) -> None:
        feature_deps = _extract_feature_deps(cf.content)
        path_deps = _extract_path_deps(cf.content)
        for dep_name in feature_deps:
            self._link_feature_dep_by_name(cf, dep_name, path_deps, cargo_by_dir, edges)

    def _link_feature_dep_by_name(
        self,
        cf: Fragment,
        dep_name: str,
        path_deps: list[tuple[str, str]],
        cargo_by_dir: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for name, rel_path in path_deps:
            if name != dep_name:
                continue
            try:
                dep_dir = str((cf.path.parent / rel_path).resolve())
            except (OSError, ValueError):
                continue
            for fid in cargo_by_dir.get(dep_dir, []):
                if fid != cf.id:
                    self.add_edge(edges, cf.id, fid, self.path_dep_weight)


def _get_default_entry_points(content: str) -> list[str]:
    entries = _extract_entry_points(content)
    defaults = ["src/lib.rs", "src/main.rs"]
    for default in defaults:
        if default not in entries:
            entries.append(default)
    return entries
