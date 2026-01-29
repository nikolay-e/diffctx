from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict

_GO_IMPORT_SINGLE_RE = re.compile(r'^\s*import\s+"([^"]+)"', re.MULTILINE)
_GO_IMPORT_BLOCK_RE = re.compile(r"import\s*\((.*?)\)", re.DOTALL)
_GO_IMPORT_LINE_RE = re.compile(r'^\s*(?:\w+\s+)?"([^"]+)"', re.MULTILINE)

_GO_FUNC_RE = re.compile(r"^func\s+(?:\([^)]+\)\s+)?(\w+)\s*\(", re.MULTILINE)
_GO_TYPE_RE = re.compile(r"^type\s+(\w+)\s+(?:struct|interface|func)", re.MULTILINE)
_GO_CONST_VAR_RE = re.compile(r"^(?:const|var)\s+(\w+)\s+", re.MULTILINE)

_GO_FUNC_CALL_RE = re.compile(r"\b([A-Z]\w+)\s*\(")
_GO_TYPE_REF_RE = re.compile(r"\*?([A-Z]\w*)\b")
_GO_PKG_CALL_RE = re.compile(r"\b(\w+)\.([A-Z]\w*)")


def _extract_imports(content: str) -> set[str]:
    imports: set[str] = set()

    for match in _GO_IMPORT_SINGLE_RE.finditer(content):
        imports.add(match.group(1))

    for block_match in _GO_IMPORT_BLOCK_RE.finditer(content):
        block = block_match.group(1)
        for line_match in _GO_IMPORT_LINE_RE.finditer(block):
            imports.add(line_match.group(1))

    return imports


def _extract_definitions(content: str) -> tuple[set[str], set[str], set[str]]:
    funcs = {m.group(1) for m in _GO_FUNC_RE.finditer(content)}
    types = {m.group(1) for m in _GO_TYPE_RE.finditer(content)}
    consts_vars = {m.group(1) for m in _GO_CONST_VAR_RE.finditer(content)}
    return funcs, types, consts_vars


def _extract_references(content: str) -> tuple[set[str], set[str], set[tuple[str, str]]]:
    func_calls = {m.group(1) for m in _GO_FUNC_CALL_RE.finditer(content) if m.group(1)[0].isupper()}
    type_refs = {m.group(1) for m in _GO_TYPE_REF_RE.finditer(content) if m.group(1)[0].isupper()}
    pkg_calls = {(m.group(1), m.group(2)) for m in _GO_PKG_CALL_RE.finditer(content)}
    return func_calls, type_refs, pkg_calls


def _is_go_file(path: Path) -> bool:
    return path.suffix.lower() == ".go"


def _get_package_name(path: Path) -> str:
    return path.parent.name


class GoEdgeBuilder(EdgeBuilder):
    weight = 0.75
    import_weight = 0.70
    type_weight = 0.65
    func_weight = 0.60
    same_package_weight = 0.55
    reverse_weight_factor = 0.4

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        go_frags = [f for f in fragments if _is_go_file(f.path)]
        if not go_frags:
            return {}

        edges: EdgeDict = {}
        indices = self._build_indices(go_frags, repo_root)

        for gf in go_frags:
            self._link_fragment(gf, indices, edges)

        return edges

    def _build_indices(
        self, go_frags: list[Fragment], repo_root: Path | None
    ) -> tuple[
        dict[str, list[FragmentId]], dict[str, list[FragmentId]], dict[str, list[FragmentId]], dict[str, list[FragmentId]]
    ]:
        pkg_to_frags: dict[str, list[FragmentId]] = defaultdict(list)
        path_to_frags: dict[str, list[FragmentId]] = defaultdict(list)
        type_defs: dict[str, list[FragmentId]] = defaultdict(list)
        func_defs: dict[str, list[FragmentId]] = defaultdict(list)

        for f in go_frags:
            pkg = _get_package_name(f.path).lower()
            pkg_to_frags[pkg].append(f.id)

            if repo_root:
                try:
                    rel = f.path.relative_to(repo_root)
                    path_to_frags[str(rel.parent)].append(f.id)
                except ValueError:
                    pass

            funcs, types, _ = _extract_definitions(f.content)
            for t in types:
                type_defs[t.lower()].append(f.id)
            for fn in funcs:
                func_defs[fn.lower()].append(f.id)

        return pkg_to_frags, path_to_frags, type_defs, func_defs

    def _link_fragment(
        self,
        gf: Fragment,
        indices: tuple[
            dict[str, list[FragmentId]], dict[str, list[FragmentId]], dict[str, list[FragmentId]], dict[str, list[FragmentId]]
        ],
        edges: EdgeDict,
    ) -> None:
        pkg_to_frags, path_to_frags, type_defs, func_defs = indices
        imports = _extract_imports(gf.content)
        func_calls, type_refs, pkg_calls = _extract_references(gf.content)

        self._link_imports(gf, imports, pkg_to_frags, path_to_frags, edges)
        self._link_refs(gf, type_refs, type_defs, self.type_weight, edges)
        self._link_refs(gf, func_calls, func_defs, self.func_weight, edges)
        self._link_pkg_calls(gf, pkg_calls, pkg_to_frags, edges)
        self._link_same_package(gf, pkg_to_frags, edges)

    def _link_imports(
        self,
        gf: Fragment,
        imports: set[str],
        pkg_to_frags: dict[str, list[FragmentId]],
        path_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for imp in imports:
            self._link_import_by_package(gf.id, imp, pkg_to_frags, edges)
            self._link_import_by_path(gf.id, imp, path_to_frags, edges)

    def _link_import_by_package(
        self,
        gf_id: FragmentId,
        imp: str,
        pkg_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        imp_pkg = imp.split("/")[-1].lower()
        for pkg, frag_ids in pkg_to_frags.items():
            if pkg == imp_pkg:
                self.add_edges_from_ids(gf_id, frag_ids, self.import_weight, edges)

    def _link_import_by_path(
        self,
        gf_id: FragmentId,
        imp: str,
        path_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for path_str, frag_ids in path_to_frags.items():
            if imp in path_str or imp.endswith(path_str):
                self.add_edges_from_ids(gf_id, frag_ids, self.import_weight, edges)

    def _link_refs(
        self,
        gf: Fragment,
        refs: set[str],
        defs: dict[str, list[FragmentId]],
        weight: float,
        edges: EdgeDict,
    ) -> None:
        for ref in refs:
            for fid in defs.get(ref.lower(), []):
                if fid != gf.id:
                    self.add_edge(edges, gf.id, fid, weight)

    def _link_pkg_calls(
        self,
        gf: Fragment,
        pkg_calls: set[tuple[str, str]],
        pkg_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for pkg_name, _symbol in pkg_calls:
            for fid in pkg_to_frags.get(pkg_name.lower(), []):
                if fid != gf.id:
                    self.add_edge(edges, gf.id, fid, self.func_weight)

    def _link_same_package(
        self,
        gf: Fragment,
        pkg_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        current_pkg = _get_package_name(gf.path).lower()
        for fid in pkg_to_frags.get(current_pkg, []):
            if fid != gf.id:
                self.add_edge(edges, gf.id, fid, self.same_package_weight)
