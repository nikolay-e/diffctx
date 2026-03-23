from __future__ import annotations

import re
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_NIX_EXTS = {".nix"}
_FLAKE_NIX = "flake.nix"
_FLAKE_LOCK = "flake.lock"
_NIX_LOCK = {_FLAKE_LOCK}

_IMPORT_RE = re.compile(r"import\s+(\.{0,2}/[^\s;{]{1,300})", re.MULTILINE)
_CALL_PACKAGE_RE = re.compile(r"callPackage\s+(\.{0,2}/[^\s;{]{1,300})", re.MULTILINE)
_IMPORT_PATH_BRACKET_RE = re.compile(r"import\s+\(\s*(\.{0,2}/[^\s)]{1,300})\s*\)", re.MULTILINE)
_IMPORTS_LIST_RE = re.compile(r"^\s*(\.{0,2}/[^\s;,\]}{]{1,300}\.nix)", re.MULTILINE)


def _is_nix_file(path: Path) -> bool:
    return path.suffix.lower() in _NIX_EXTS or path.name in _NIX_LOCK


def _extract_import_paths(content: str) -> set[str]:
    paths: set[str] = set()
    for m in _IMPORT_RE.finditer(content):
        paths.add(m.group(1))
    for m in _CALL_PACKAGE_RE.finditer(content):
        paths.add(m.group(1))
    for m in _IMPORT_PATH_BRACKET_RE.finditer(content):
        paths.add(m.group(1))
    for m in _IMPORTS_LIST_RE.finditer(content):
        paths.add(m.group(1))
    return paths


def _ref_to_filename(ref: str) -> str:
    name = ref.rstrip("/").split("/")[-1].lower()
    if not name.endswith(".nix") and not name.endswith(".lock"):
        name = name + ".nix"
    return name


class NixEdgeBuilder(EdgeBuilder):
    weight = 0.60
    import_weight = EDGE_WEIGHTS["nix_import"].forward
    reverse_weight_factor = EDGE_WEIGHTS["nix_import"].reverse_factor

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        nix_changed = [f for f in changed_files if _is_nix_file(f)]
        if not nix_changed:
            return []

        refs: set[str] = set()
        for f in nix_changed:
            try:
                content = f.read_text(encoding="utf-8")
                for imp in _extract_import_paths(content):
                    refs.add(_ref_to_filename(imp))
                    refs.add(imp)

                if f.name == _FLAKE_NIX:
                    refs.add(_FLAKE_LOCK)
                if f.name == _FLAKE_LOCK:
                    refs.add(_FLAKE_NIX)
            except (OSError, UnicodeDecodeError):
                continue

        self._discover_reverse_imports(nix_changed, all_candidate_files, refs, repo_root)

        return discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

    def _discover_reverse_imports(
        self,
        nix_changed: list[Path],
        all_candidate_files: list[Path],
        refs: set[str],
        repo_root: Path | None,
    ) -> None:
        changed_paths = self._collect_changed_paths(nix_changed, repo_root)
        for candidate in all_candidate_files:
            if not _is_nix_file(candidate):
                continue
            try:
                content = candidate.read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                continue
            if self._candidate_imports_changed_path(content, changed_paths):
                refs.add(candidate.name.lower())

    @staticmethod
    def _collect_changed_paths(nix_changed: list[Path], repo_root: Path | None) -> set[str]:
        changed_paths: set[str] = set()
        for f in nix_changed:
            changed_paths.add(f.name.lower())
            if repo_root:
                try:
                    rel = f.relative_to(repo_root)
                    changed_paths.add(str(rel))
                    changed_paths.add(rel.as_posix())
                except ValueError:
                    pass
        return changed_paths

    @staticmethod
    def _candidate_imports_changed_path(content: str, changed_paths: set[str]) -> bool:
        for imp in _extract_import_paths(content):
            imp_name = _ref_to_filename(imp)
            for cp in changed_paths:
                if cp == imp_name or cp in imp:
                    return True
        return False

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        nix_frags = [f for f in fragments if _is_nix_file(f.path)]
        if not nix_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)

        for nf in nix_frags:
            self._add_fragment_edges(nf, idx, edges)

        self._link_flake_pairs(nix_frags, edges)

        return edges

    def _add_fragment_edges(self, nf: Fragment, idx: FragmentIndex, edges: EdgeDict) -> None:
        import_paths = _extract_import_paths(nf.content)
        for imp in import_paths:
            self._link_import(nf.id, imp, nf.path, idx, edges)

    def _link_import(
        self,
        src_id: FragmentId,
        imp: str,
        source_path: Path,
        idx: FragmentIndex,
        edges: EdgeDict,
    ) -> None:
        filename = _ref_to_filename(imp)
        for name, frag_ids in idx.by_name.items():
            if name == filename:
                for fid in frag_ids:
                    if fid != src_id:
                        self.add_edge(edges, src_id, fid, self.import_weight)
                        return

        resolved = str(source_path.parent / imp)
        self.link_by_path_match(src_id, resolved, idx, edges, self.import_weight)
        self.link_by_path_match(src_id, imp, idx, edges, self.import_weight)

    def _link_flake_pairs(self, nix_frags: list[Fragment], edges: EdgeDict) -> None:
        flake_nix: list[Fragment] = []
        flake_lock: list[Fragment] = []
        for f in nix_frags:
            if f.path.name == _FLAKE_NIX:
                flake_nix.append(f)
            elif f.path.name == _FLAKE_LOCK:
                flake_lock.append(f)

        for fn in flake_nix:
            for fl in flake_lock:
                if fn.path.parent == fl.path.parent:
                    self.add_edge(edges, fn.id, fl.id, self.import_weight)
