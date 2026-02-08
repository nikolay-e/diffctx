from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict

_CSHARP_EXTS = {".cs"}
_FSHARP_EXTS = {".fs", ".fsi", ".fsx"}
_DOTNET_EXTS = _CSHARP_EXTS | _FSHARP_EXTS

_CS_USING_RE = re.compile(r"^\s*using\s+(?:static\s+)?([A-Z][a-zA-Z0-9_.]*);", re.MULTILINE)
_CS_NAMESPACE_RE = re.compile(r"^\s*namespace\s+([A-Z][a-zA-Z0-9_.]*)", re.MULTILINE)
_CS_ACCESS = "public|private|protected|internal"
_CS_MODIFIERS = "static|sealed|abstract|partial"
_CS_TYPE_KW = "class|interface|struct|record|enum"
_CS_CLASS_RE = re.compile(
    rf"^\s{{0,20}}(?:(?:{_CS_ACCESS})\s{{1,10}})?(?:(?:{_CS_MODIFIERS})\s{{1,10}})?(?:{_CS_TYPE_KW})\s+([A-Z]\w{{0,100}})",
    re.MULTILINE,
)
_CS_INHERIT_RE = re.compile(r"(?:class|struct|record)\s+\w+[^:\n]{0,200}:\s*([A-Z]\w*(?:,\s*[A-Z]\w*)*)")
_CS_GENERIC_RE = re.compile(r"<([A-Z]\w*(?:\s*,\s*[A-Z]\w*)*)>")
_CS_METHOD_RE = re.compile(
    r"^\s*(?:public |private |protected |internal )?(?:\w+ )?[A-Z]\w*(?:<[^>]+>)? ([A-Z]\w*)\s*\(",
    re.MULTILINE,
)

_FS_OPEN_RE = re.compile(r"^\s*open\s+([A-Z][a-zA-Z0-9_.]*)", re.MULTILINE)
_FS_MODULE_RE = re.compile(r"^\s*module\s+(?:rec\s+)?([A-Z][a-zA-Z0-9_.]*)", re.MULTILINE)
_FS_NAMESPACE_RE = re.compile(r"^\s*namespace\s+(?:rec\s+)?([A-Z][a-zA-Z0-9_.]*)", re.MULTILINE)
_FS_TYPE_RE = re.compile(r"^\s*type\s+(?:private\s+)?([A-Z]\w*)", re.MULTILINE)
_FS_LET_RE = re.compile(r"^\s*let\s+(?:rec\s+)?(?:private\s+)?([a-z]\w*)", re.MULTILINE)

_TYPE_REF_RE = re.compile(r"(?<![a-z_])([A-Z]\w*)\b")
_ATTRIBUTE_RE = re.compile(r"\[([A-Z]\w*)\]")


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


class DotNetEdgeBuilder(EdgeBuilder):
    weight = 0.70
    using_weight = 0.75
    inheritance_weight = 0.80
    type_weight = 0.60
    same_namespace_weight = 0.55
    attribute_weight = 0.50
    partial_class_weight = 0.85
    reverse_weight_factor = 0.4

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
