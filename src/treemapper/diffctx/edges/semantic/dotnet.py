from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, read_cached

_CSHARP_EXTS = {".cs"}
_FSHARP_EXTS = {".fs", ".fsi", ".fsx"}
_DOTNET_EXTS = _CSHARP_EXTS | _FSHARP_EXTS

_CS_USING_RE = re.compile(r"^\s*(?:global\s+)?using\s+(?:static\s+)?([A-Z][a-zA-Z0-9_.]*);", re.MULTILINE)
_CS_NAMESPACE_RE = re.compile(r"^\s*namespace\s+([A-Z][a-zA-Z0-9_.]*)", re.MULTILINE)
_CS_ACCESS = "public|private|protected|internal"
_CS_MODIFIERS = "static|sealed|abstract|partial"
_CS_TYPE_KW = "class|interface|struct|record|enum"
_CS_CLASS_RE = re.compile(
    rf"^\s{{0,20}}(?:(?:{_CS_ACCESS})\s{{1,10}})?(?:(?:{_CS_MODIFIERS})\s{{1,10}})*(?:{_CS_TYPE_KW})\s+([A-Z]\w{{0,100}})",
    re.MULTILINE,
)
_CS_INHERIT_RE = re.compile(
    r"(?:class|struct|record)\s+\w+[^{]{0,300}?:\s*((?:[A-Z]\w*(?:\s*,\s*)?)+)",
    re.DOTALL,
)
_FS_OPEN_RE = re.compile(r"^\s*open\s+([A-Z][a-zA-Z0-9_.]*)", re.MULTILINE)
_FS_MODULE_RE = re.compile(r"^\s*module\s+(?:rec\s+)?([A-Z][a-zA-Z0-9_.]*)", re.MULTILINE)
_FS_NAMESPACE_RE = re.compile(r"^\s*namespace\s+(?:rec\s+)?([A-Z][a-zA-Z0-9_.]*)", re.MULTILINE)
_FS_TYPE_RE = re.compile(r"^\s*type\s+(?:private\s+)?([A-Z]\w*)", re.MULTILINE)
_FS_LET_RE = re.compile(r"^\s*let\s+(?:rec\s+)?(?:private\s+)?([a-z]\w*)", re.MULTILINE)

_TYPE_REF_RE = re.compile(r"(?<![a-z_])([A-Z]\w*)\b")
_ATTRIBUTE_RE = re.compile(r"\[([A-Z]\w*)[\](]")


def _is_dotnet_file(path: Path) -> bool:
    return path.suffix.lower() in _DOTNET_EXTS


def _is_csharp(path: Path) -> bool:
    return path.suffix.lower() in _CSHARP_EXTS


def _is_fsharp(path: Path) -> bool:
    return path.suffix.lower() in _FSHARP_EXTS


def _extract_usings(content: str, path: Path) -> set[str]:
    if _is_csharp(path):
        return {m.group(1) for m in _CS_USING_RE.finditer(content)}
    elif _is_fsharp(path):
        return {m.group(1) for m in _FS_OPEN_RE.finditer(content)}
    return set()


def _extract_namespace(content: str, path: Path) -> str | None:
    if _is_csharp(path):
        match = _CS_NAMESPACE_RE.search(content)
    else:
        match = _FS_NAMESPACE_RE.search(content) or _FS_MODULE_RE.search(content)
    return match.group(1) if match else None


def _extract_types(content: str, path: Path) -> set[str]:
    types: set[str] = set()
    if _is_csharp(path):
        types.update(m.group(1) for m in _CS_CLASS_RE.finditer(content))
    elif _is_fsharp(path):
        types.update(m.group(1) for m in _FS_TYPE_RE.finditer(content))
    return types


def _extract_inheritance(content: str) -> set[str]:
    refs: set[str] = set()
    for m in _CS_INHERIT_RE.finditer(content):
        for cls in m.group(1).split(","):
            refs.add(cls.strip())
    return refs


def _extract_type_refs(content: str) -> set[str]:
    return {m.group(1) for m in _TYPE_REF_RE.finditer(content)}


def _extract_attributes(content: str) -> set[str]:
    return {m.group(1) for m in _ATTRIBUTE_RE.finditer(content)}


_DISCOVERY_MAX_DEPTH = 2


class DotNetEdgeBuilder(EdgeBuilder):
    weight = 0.70
    using_weight = EDGE_WEIGHTS["dotnet_using"].forward
    inheritance_weight = EDGE_WEIGHTS["dotnet_inheritance"].forward
    type_weight = EDGE_WEIGHTS["dotnet_type"].forward
    same_namespace_weight = EDGE_WEIGHTS["dotnet_same_namespace"].forward
    attribute_weight = EDGE_WEIGHTS["dotnet_attribute"].forward
    partial_class_weight = EDGE_WEIGHTS["dotnet_partial"].forward
    reverse_weight_factor = EDGE_WEIGHTS["dotnet_using"].reverse_factor

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        dotnet_changed = [f for f in changed_files if _is_dotnet_file(f)]
        if not dotnet_changed:
            return []

        fc = kwargs.get("file_cache")
        cache: dict[Path, str] | None = fc if isinstance(fc, dict) else None

        dotnet_candidates = [f for f in all_candidate_files if _is_dotnet_file(f)]
        file_types: dict[Path, set[str]] = {}
        file_usings_full: dict[Path, set[str]] = {}
        file_type_refs: dict[Path, set[str]] = {}
        file_namespaces: dict[Path, str | None] = {}

        for candidate in dotnet_candidates:
            content = read_cached(candidate, cache)
            if content is None:
                continue
            file_types[candidate] = {t.lower() for t in _extract_types(content, candidate)}
            file_usings_full[candidate] = {u.lower() for u in _extract_usings(content, candidate)}
            file_type_refs[candidate] = {t.lower() for t in _extract_type_refs(content)}
            file_namespaces[candidate] = _extract_namespace(content, candidate)

        changed_set = set(changed_files)
        discovered: set[Path] = set()
        frontier = set(dotnet_changed)

        for _depth in range(_DISCOVERY_MAX_DEPTH):
            frontier_types, frontier_ns = self._collect_frontier_refs(frontier, cache)
            frontier = self._discover_one_hop(
                dotnet_candidates,
                changed_set | discovered,
                frontier_types,
                frontier_ns,
                file_usings_full,
                file_type_refs,
                file_namespaces,
            )
            discovered.update(frontier)
            if not frontier:
                break

        return list(discovered)

    @staticmethod
    def _collect_frontier_refs(
        frontier: set[Path],
        cache: dict[Path, str] | None,
    ) -> tuple[set[str], set[str]]:
        types: set[str] = set()
        namespaces: set[str] = set()
        for f in frontier:
            content = read_cached(f, cache)
            if content is None:
                continue
            types.update(t.lower() for t in _extract_types(content, f))
            ns = _extract_namespace(content, f)
            if ns:
                namespaces.add(ns.lower())
        return types, namespaces

    @staticmethod
    def _discover_one_hop(
        candidates: list[Path],
        skip: set[Path],
        frontier_types: set[str],
        frontier_ns: set[str],
        file_usings: dict[Path, set[str]],
        file_type_refs: dict[Path, set[str]],
        file_namespaces: dict[Path, str | None],
    ) -> set[Path]:
        found: set[Path] = set()
        for candidate in candidates:
            if candidate in skip:
                continue
            if file_usings.get(candidate, set()) & frontier_ns:
                found.add(candidate)
                continue
            if file_type_refs.get(candidate, set()) & frontier_types:
                found.add(candidate)
                continue
            cand_ns = file_namespaces.get(candidate)
            if cand_ns and cand_ns.lower() in frontier_ns:
                found.add(candidate)
        return found

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        dotnet_frags = [f for f in fragments if _is_dotnet_file(f.path)]
        if not dotnet_frags:
            return {}

        edges: EdgeDict = {}
        indices = self._build_indices(dotnet_frags)

        for df in dotnet_frags:
            self._link_fragment(df, indices, edges)

        return edges

    def _build_indices(
        self, dotnet_frags: list[Fragment]
    ) -> tuple[dict[str, list[FragmentId]], dict[str, list[FragmentId]], dict[str, list[FragmentId]]]:
        namespace_to_frags: dict[str, list[FragmentId]] = defaultdict(list)
        type_to_frags: dict[str, list[FragmentId]] = defaultdict(list)
        fqn_to_frags: dict[str, list[FragmentId]] = defaultdict(list)

        for f in dotnet_frags:
            ns = _extract_namespace(f.content, f.path)
            if ns:
                ns_parts = ns.split(".")
                for i in range(len(ns_parts)):
                    partial_ns = ".".join(ns_parts[: i + 1])
                    namespace_to_frags[partial_ns.lower()].append(f.id)

            for t in _extract_types(f.content, f.path):
                type_to_frags[t.lower()].append(f.id)
                if ns:
                    fqn_to_frags[f"{ns}.{t}".lower()].append(f.id)

        return namespace_to_frags, type_to_frags, fqn_to_frags

    def _link_fragment(
        self,
        df: Fragment,
        indices: tuple[dict[str, list[FragmentId]], dict[str, list[FragmentId]], dict[str, list[FragmentId]]],
        edges: EdgeDict,
    ) -> None:
        namespace_to_frags, type_to_frags, fqn_to_frags = indices

        self._link_usings(df, namespace_to_frags, fqn_to_frags, type_to_frags, edges)
        self._link_refs(df, type_to_frags, edges)
        self._link_same_namespace(df, namespace_to_frags, edges)
        self._link_partial_classes(df, type_to_frags, edges)

    def _link_usings(
        self,
        df: Fragment,
        namespace_to_frags: dict[str, list[FragmentId]],
        fqn_to_frags: dict[str, list[FragmentId]],
        type_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for using in _extract_usings(df.content, df.path):
            self._link_single_using(df.id, using, namespace_to_frags, fqn_to_frags, type_to_frags, edges)

    def _link_single_using(
        self,
        df_id: FragmentId,
        using: str,
        namespace_to_frags: dict[str, list[FragmentId]],
        fqn_to_frags: dict[str, list[FragmentId]],
        type_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        using_lower = using.lower()
        self.add_edges_from_ids(df_id, namespace_to_frags.get(using_lower, []), self.using_weight, edges)
        self.add_edges_from_ids(df_id, fqn_to_frags.get(using_lower, []), self.using_weight, edges)

        parts = using.split(".")
        if parts:
            self.add_edges_from_ids(df_id, type_to_frags.get(parts[-1].lower(), []), self.using_weight, edges)

    def _link_refs(
        self,
        df: Fragment,
        type_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        self._link_inheritance(df.id, df.content, type_to_frags, edges)
        self._link_type_refs(df.id, df.content, type_to_frags, edges)
        self._link_attributes(df.id, df.content, type_to_frags, edges)

    def _link_inheritance(
        self, df_id: FragmentId, content: str, type_to_frags: dict[str, list[FragmentId]], edges: EdgeDict
    ) -> None:
        for parent in _extract_inheritance(content):
            self.add_edges_from_ids(df_id, type_to_frags.get(parent.lower(), []), self.inheritance_weight, edges)

    def _link_type_refs(
        self, df_id: FragmentId, content: str, type_to_frags: dict[str, list[FragmentId]], edges: EdgeDict
    ) -> None:
        for type_ref in _extract_type_refs(content):
            self.add_edges_from_ids(df_id, type_to_frags.get(type_ref.lower(), []), self.type_weight, edges)

    def _link_attributes(
        self, df_id: FragmentId, content: str, type_to_frags: dict[str, list[FragmentId]], edges: EdgeDict
    ) -> None:
        for attr in _extract_attributes(content):
            attr_lower = attr.lower()
            attr_full = attr_lower if attr_lower.endswith("attribute") else attr_lower + "attribute"
            for lookup in [attr_lower, attr_full]:
                self.add_edges_from_ids(df_id, type_to_frags.get(lookup, []), self.attribute_weight, edges)

    def _link_same_namespace(
        self,
        df: Fragment,
        namespace_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        current_ns = _extract_namespace(df.content, df.path)
        if not current_ns:
            return
        for fid in namespace_to_frags.get(current_ns.lower(), []):
            if fid != df.id:
                self.add_edge(edges, df.id, fid, self.same_namespace_weight)

    def _link_partial_classes(
        self,
        df: Fragment,
        type_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for current_type in _extract_types(df.content, df.path):
            for fid in type_to_frags.get(current_type.lower(), []):
                if fid != df.id:
                    self.add_edge(edges, df.id, fid, self.partial_class_weight)
