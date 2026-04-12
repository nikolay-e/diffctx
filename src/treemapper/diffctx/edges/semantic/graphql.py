from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_GRAPHQL_EXTS = {".graphql", ".gql"}

_GQL_IMPORT_RE = re.compile(r'^\s{0,20}#\s*import\s{1,10}"([^"]{1,300})"', re.MULTILINE)

_GQL_TYPE_DEF_RE = re.compile(r"^\s{0,20}type\s{1,10}([A-Z]\w{0,100})\b", re.MULTILINE)
_GQL_INPUT_DEF_RE = re.compile(r"^\s{0,20}input\s{1,10}([A-Z]\w{0,100})\b", re.MULTILINE)
_GQL_INTERFACE_DEF_RE = re.compile(r"^\s{0,20}interface\s{1,10}([A-Z]\w{0,100})\b", re.MULTILINE)
_GQL_ENUM_DEF_RE = re.compile(r"^\s{0,20}enum\s{1,10}([A-Z]\w{0,100})\b", re.MULTILINE)
_GQL_SCALAR_DEF_RE = re.compile(r"^\s{0,20}scalar\s{1,10}([A-Z]\w{0,100})\b", re.MULTILINE)

_GQL_EXTEND_TYPE_RE = re.compile(r"^\s{0,20}extend\s{1,10}type\s{1,10}([A-Z]\w{0,100})\b", re.MULTILINE)
_GQL_EXTEND_INPUT_RE = re.compile(r"^\s{0,20}extend\s{1,10}input\s{1,10}([A-Z]\w{0,100})\b", re.MULTILINE)
_GQL_EXTEND_INTERFACE_RE = re.compile(r"^\s{0,20}extend\s{1,10}interface\s{1,10}([A-Z]\w{0,100})\b", re.MULTILINE)

_GQL_IMPLEMENTS_RE = re.compile(r"implements\s{1,10}([A-Z][\w\s&]{0,300}?)(?:\s*\{|\s*@)", re.MULTILINE)
_GQL_UNION_RE = re.compile(r"^\s{0,20}union\s{1,10}([A-Z]\w{0,100})\s*=\s*([A-Z][\w\s|]{0,500})", re.MULTILINE)

_GQL_FIELD_TYPE_RE = re.compile(r":\s*(?:\[\s*)?([A-Z]\w{0,100})")

_GQL_BUILTIN_TYPES = frozenset(
    {
        "String",
        "Int",
        "Float",
        "Boolean",
        "ID",
    }
)


def _is_graphql_file(path: Path) -> bool:
    return path.suffix.lower() in _GRAPHQL_EXTS


def _extract_imports(content: str) -> set[str]:
    return {m.group(1) for m in _GQL_IMPORT_RE.finditer(content)}


def _extract_type_definitions(content: str) -> set[str]:
    defs: set[str] = set()
    defs.update(m.group(1) for m in _GQL_TYPE_DEF_RE.finditer(content))
    defs.update(m.group(1) for m in _GQL_INPUT_DEF_RE.finditer(content))
    defs.update(m.group(1) for m in _GQL_INTERFACE_DEF_RE.finditer(content))
    defs.update(m.group(1) for m in _GQL_ENUM_DEF_RE.finditer(content))
    defs.update(m.group(1) for m in _GQL_SCALAR_DEF_RE.finditer(content))
    defs.update(m.group(1) for m in _GQL_UNION_RE.finditer(content))
    return defs


def _extract_extended_types(content: str) -> set[str]:
    extended: set[str] = set()
    extended.update(m.group(1) for m in _GQL_EXTEND_TYPE_RE.finditer(content))
    extended.update(m.group(1) for m in _GQL_EXTEND_INPUT_RE.finditer(content))
    extended.update(m.group(1) for m in _GQL_EXTEND_INTERFACE_RE.finditer(content))
    return extended


def _extract_implemented_interfaces(content: str) -> set[str]:
    interfaces: set[str] = set()
    for m in _GQL_IMPLEMENTS_RE.finditer(content):
        raw = m.group(1)
        for part in raw.split("&"):
            name = part.strip()
            if name and name[0].isupper():
                interfaces.add(name)
    return interfaces


def _extract_union_members(content: str) -> set[str]:
    members: set[str] = set()
    for m in _GQL_UNION_RE.finditer(content):
        raw = m.group(2)
        for part in raw.split("|"):
            name = part.strip()
            if name and name[0].isupper():
                members.add(name)
    return members


def _extract_field_type_refs(content: str) -> set[str]:
    refs: set[str] = set()
    for m in _GQL_FIELD_TYPE_RE.finditer(content):
        name = m.group(1)
        if name not in _GQL_BUILTIN_TYPES:
            refs.add(name)
    return refs


def _collect_graphql_refs(gql_files: list[Path]) -> set[str]:
    refs: set[str] = set()
    for gf in gql_files:
        try:
            content = gf.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        refs.update(_extract_imports(content))
    return refs


class GraphqlEdgeBuilder(EdgeBuilder):
    weight = 0.60
    import_weight = EDGE_WEIGHTS["graphql_import"].forward
    type_ref_weight = EDGE_WEIGHTS["graphql_type_ref"].forward
    extend_weight = EDGE_WEIGHTS["graphql_extend"].forward
    reverse_weight_factor = EDGE_WEIGHTS["graphql_import"].reverse_factor

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        gql_changed = [f for f in changed_files if _is_graphql_file(f)]
        if not gql_changed:
            return []

        refs = _collect_graphql_refs(gql_changed)
        discovered = discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

        changed_set = set(changed_files)
        changed_types = self._collect_defined_types(gql_changed)
        if changed_types:
            discovered.extend(self._find_files_referencing_types(all_candidate_files, changed_set, changed_types))

        return list(set(discovered))

    def _collect_defined_types(self, gql_files: list[Path]) -> set[str]:
        types: set[str] = set()
        for gf in gql_files:
            try:
                content = gf.read_text(encoding="utf-8")
                types.update(_extract_type_definitions(content))
            except (OSError, UnicodeDecodeError):
                continue
        return types

    def _find_files_referencing_types(
        self, all_candidate_files: list[Path], changed_set: set[Path], target_types: set[str]
    ) -> list[Path]:
        target_lower = {t.lower() for t in target_types}
        discovered: list[Path] = []
        for candidate in all_candidate_files:
            if candidate in changed_set or not _is_graphql_file(candidate):
                continue
            try:
                content = candidate.read_text(encoding="utf-8")
                refs = _extract_field_type_refs(content)
                refs.update(_extract_extended_types(content))
                refs.update(_extract_implemented_interfaces(content))
                refs.update(_extract_union_members(content))
                if any(r.lower() in target_lower for r in refs):
                    discovered.append(candidate)
            except (OSError, UnicodeDecodeError):
                continue
        return discovered

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        gql_frags = [f for f in fragments if _is_graphql_file(f.path)]
        if not gql_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)
        type_defs = self._build_type_index(gql_frags)

        for gf in gql_frags:
            self._add_import_edges(gf, idx, edges)
            self._add_type_ref_edges(gf, type_defs, edges)
            self._add_extend_edges(gf, type_defs, edges)
            self._add_interface_edges(gf, type_defs, edges)
            self._add_union_edges(gf, type_defs, edges)

        return edges

    def _build_type_index(self, gql_frags: list[Fragment]) -> dict[str, list[FragmentId]]:
        defs: dict[str, list[FragmentId]] = defaultdict(list)
        for f in gql_frags:
            for name in _extract_type_definitions(f.content):
                defs[name.lower()].append(f.id)
        return defs

    def _add_import_edges(self, gf: Fragment, idx: FragmentIndex, edges: EdgeDict) -> None:
        for imp in _extract_imports(gf.content):
            self.link_by_path_match(gf.id, imp, idx, edges, self.import_weight)

    def _add_type_ref_edges(self, gf: Fragment, type_defs: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        own_defs = {n.lower() for n in _extract_type_definitions(gf.content)}
        for ref in _extract_field_type_refs(gf.content):
            ref_lower = ref.lower()
            if ref_lower in own_defs:
                continue
            for fid in type_defs.get(ref_lower, []):
                if fid != gf.id:
                    self.add_edge(edges, gf.id, fid, self.type_ref_weight)

    def _add_extend_edges(self, gf: Fragment, type_defs: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        for ext_name in _extract_extended_types(gf.content):
            for fid in type_defs.get(ext_name.lower(), []):
                if fid != gf.id:
                    self.add_edge(edges, gf.id, fid, self.extend_weight)

    def _add_interface_edges(self, gf: Fragment, type_defs: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        for iface in _extract_implemented_interfaces(gf.content):
            for fid in type_defs.get(iface.lower(), []):
                if fid != gf.id:
                    self.add_edge(edges, gf.id, fid, self.type_ref_weight)

    def _add_union_edges(self, gf: Fragment, type_defs: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        for member in _extract_union_members(gf.content):
            for fid in type_defs.get(member.lower(), []):
                if fid != gf.id:
                    self.add_edge(edges, gf.id, fid, self.type_ref_weight)
