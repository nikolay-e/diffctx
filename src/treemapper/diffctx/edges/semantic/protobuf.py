from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_PROTO_EXTS = {".proto"}

_PROTO_IMPORT_RE = re.compile(r'^\s{0,20}import\s{1,10}(?:public\s{1,10})?"([^"]{1,300})"\s*;', re.MULTILINE)
_PROTO_PACKAGE_RE = re.compile(r"^\s{0,20}package\s{1,10}([a-zA-Z_][\w.]{0,200})\s*;", re.MULTILINE)
_PROTO_MESSAGE_RE = re.compile(r"^\s{0,20}message\s{1,10}([A-Z]\w{0,100})\s*\{", re.MULTILINE)
_PROTO_ENUM_RE = re.compile(r"^\s{0,20}enum\s{1,10}([A-Z]\w{0,100})\s*\{", re.MULTILINE)
_PROTO_SERVICE_RE = re.compile(r"^\s{0,20}service\s{1,10}([A-Z]\w{0,100})\s*\{", re.MULTILINE)
_PROTO_RPC_RE = re.compile(
    r"^\s{0,20}rpc\s{1,10}(\w{1,100})\s*\(\s*(?:stream\s+)?([A-Z][\w.]{0,200})\s*\)"
    r"\s*returns\s*\(\s*(?:stream\s+)?([A-Z][\w.]{0,200})\s*\)",
    re.MULTILINE,
)
_PROTO_FIELD_TYPE_RE = re.compile(
    r"^\s{0,20}(?:repeated\s{1,10}|optional\s{1,10}|required\s{1,10})?"
    r"(?:map\s*<\s*\w+\s*,\s*)?([A-Z][\w.]{0,200})\s*>?\s+\w{1,100}\s*=",
    re.MULTILINE,
)
_PROTO_GO_PACKAGE_RE = re.compile(r'^\s{0,20}option\s{1,10}go_package\s*=\s*"([^"]{1,300})"\s*;', re.MULTILINE)
_PROTO_JAVA_PACKAGE_RE = re.compile(r'^\s{0,20}option\s{1,10}java_package\s*=\s*"([^"]{1,300})"\s*;', re.MULTILINE)


def _is_proto_file(path: Path) -> bool:
    return path.suffix.lower() in _PROTO_EXTS


def _extract_imports(content: str) -> set[str]:
    return {m.group(1) for m in _PROTO_IMPORT_RE.finditer(content)}


def _extract_package(content: str) -> str | None:
    m = _PROTO_PACKAGE_RE.search(content)
    return m.group(1) if m else None


def _extract_message_names(content: str) -> set[str]:
    names: set[str] = set()
    names.update(m.group(1) for m in _PROTO_MESSAGE_RE.finditer(content))
    names.update(m.group(1) for m in _PROTO_ENUM_RE.finditer(content))
    return names


def _extract_service_names(content: str) -> set[str]:
    return {m.group(1) for m in _PROTO_SERVICE_RE.finditer(content)}


def _extract_rpc_types(content: str) -> set[str]:
    types: set[str] = set()
    for m in _PROTO_RPC_RE.finditer(content):
        types.add(m.group(2).split(".")[-1])
        types.add(m.group(3).split(".")[-1])
    return types


def _extract_field_types(content: str) -> set[str]:
    types: set[str] = set()
    for m in _PROTO_FIELD_TYPE_RE.finditer(content):
        raw = m.group(1)
        types.add(raw.split(".")[-1])
    return types


_PROTO_BUILTIN_TYPES = frozenset(
    {
        "Any",
        "Timestamp",
        "Duration",
        "Empty",
        "FieldMask",
        "Struct",
        "Value",
        "ListValue",
        "BoolValue",
        "BytesValue",
        "DoubleValue",
        "FloatValue",
        "Int32Value",
        "Int64Value",
        "StringValue",
        "UInt32Value",
        "UInt64Value",
    }
)


def _extract_option_paths(content: str) -> set[str]:
    refs: set[str] = set()
    for m in _PROTO_GO_PACKAGE_RE.finditer(content):
        pkg = m.group(1)
        last_segment = pkg.rstrip("/").split("/")[-1]
        if ";" in last_segment:
            last_segment = last_segment.split(";")[0].split("/")[-1]
        refs.add(last_segment)
    for m in _PROTO_JAVA_PACKAGE_RE.finditer(content):
        pkg = m.group(1)
        last_segment = pkg.split(".")[-1]
        refs.add(last_segment)
    return refs


def _collect_proto_refs(proto_files: list[Path]) -> set[str]:
    refs: set[str] = set()
    for pf in proto_files:
        try:
            content = pf.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        refs.update(_extract_imports(content))
        refs.update(_extract_option_paths(content))
    return refs


class ProtobufEdgeBuilder(EdgeBuilder):
    weight = 0.65
    import_weight = EDGE_WEIGHTS["proto_import"].forward
    message_ref_weight = EDGE_WEIGHTS["proto_message_ref"].forward
    service_rpc_weight = EDGE_WEIGHTS["proto_service_rpc"].forward
    reverse_weight_factor = EDGE_WEIGHTS["proto_import"].reverse_factor

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        proto_changed = [f for f in changed_files if _is_proto_file(f)]
        if not proto_changed:
            return []

        refs = _collect_proto_refs(proto_changed)
        return discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        proto_frags = [f for f in fragments if _is_proto_file(f.path)]
        if not proto_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)
        message_defs = self._build_message_index(proto_frags)

        for pf in proto_frags:
            self._add_import_edges(pf, idx, edges)
            self._add_type_ref_edges(pf, message_defs, edges)
            self._add_option_path_edges(pf, idx, edges)

        return edges

    def _build_message_index(self, proto_frags: list[Fragment]) -> dict[str, list[FragmentId]]:
        defs: dict[str, list[FragmentId]] = defaultdict(list)
        for f in proto_frags:
            pkg = _extract_package(f.content)
            for name in _extract_message_names(f.content):
                defs[name.lower()].append(f.id)
                if pkg:
                    defs[f"{pkg}.{name}".lower()].append(f.id)
            for name in _extract_service_names(f.content):
                defs[name.lower()].append(f.id)
                if pkg:
                    defs[f"{pkg}.{name}".lower()].append(f.id)
        return defs

    def _add_import_edges(self, pf: Fragment, idx: FragmentIndex, edges: EdgeDict) -> None:
        for imp in _extract_imports(pf.content):
            self.link_by_path_match(pf.id, imp, idx, edges, self.import_weight)

    def _add_resolved_type_edges(
        self,
        pf: Fragment,
        type_names: set[str],
        message_defs: dict[str, list[FragmentId]],
        own_defs_lower: set[str],
        weight: float,
        edges: EdgeDict,
    ) -> None:
        for type_name in type_names:
            if type_name in _PROTO_BUILTIN_TYPES:
                continue
            type_lower = type_name.lower()
            if type_lower in own_defs_lower:
                continue
            for fid in message_defs.get(type_lower, []):
                if fid != pf.id:
                    self.add_edge(edges, pf.id, fid, weight)

    def _add_type_ref_edges(self, pf: Fragment, message_defs: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        own_defs_lower = {n.lower() for n in _extract_message_names(pf.content)}

        self._add_resolved_type_edges(
            pf, _extract_field_types(pf.content), message_defs, own_defs_lower, self.message_ref_weight, edges
        )
        self._add_resolved_type_edges(
            pf, _extract_rpc_types(pf.content), message_defs, own_defs_lower, self.service_rpc_weight, edges
        )

    def _add_option_path_edges(self, pf: Fragment, idx: FragmentIndex, edges: EdgeDict) -> None:
        for ref in _extract_option_paths(pf.content):
            self.link_by_path_match(pf.id, ref, idx, edges, self.weight * 0.6)
