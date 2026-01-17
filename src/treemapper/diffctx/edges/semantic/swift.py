from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict

_SWIFT_IMPORT_RE = re.compile(r"^\s*import\s+(\w+)", re.MULTILINE)

_SWIFT_CLASS_RE = re.compile(r"^\s*(?:\w+\s+)*class\s+(\w+)", re.MULTILINE)
_SWIFT_STRUCT_RE = re.compile(r"^\s*(?:public\s+|private\s+|internal\s+|fileprivate\s+)*struct\s+(\w+)", re.MULTILINE)
_SWIFT_ENUM_RE = re.compile(r"^\s*(?:public\s+|private\s+|internal\s+|fileprivate\s+)*enum\s+(\w+)", re.MULTILINE)
_SWIFT_PROTOCOL_RE = re.compile(r"^\s*(?:public\s+|private\s+|internal\s+|fileprivate\s+)*protocol\s+(\w+)", re.MULTILINE)
_SWIFT_EXTENSION_RE = re.compile(r"^\s*(?:public\s+|private\s+|internal\s+|fileprivate\s+)*extension\s+(\w+)", re.MULTILINE)
_SWIFT_FUNC_RE = re.compile(r"^\s*(?:\w+\s+|@\w+\s+)*func\s+(\w+)\s*[(]", re.MULTILINE)
_SWIFT_TYPEALIAS_RE = re.compile(r"^\s*(?:public\s+|private\s+|internal\s+|fileprivate\s+)*typealias\s+(\w+)", re.MULTILINE)

_SWIFT_CONFORMANCE_RE = re.compile(
    r"(?:class|struct|enum)\s{1,10}\w{1,100}\s{0,10}(?:<[^>]{1,200}>)?\s{0,10}:\s{0,10}([^{\n]{1,500})", re.MULTILINE
)
_SWIFT_TYPE_REF_RE = re.compile(r"(?<![a-z_])([A-Z]\w{0,100})\b")
_SWIFT_FUNC_CALL_RE = re.compile(r"(?<!\w)([a-z]\w{0,100})\s{0,10}\(")
_SWIFT_DOT_CALL_RE = re.compile(r"(\w+)\.([a-z][a-zA-Z0-9]*)\s*\(")


def _is_swift_file(path: Path) -> bool:
    return path.suffix.lower() == ".swift"


def _extract_imports(content: str) -> set[str]:
    return {m.group(1) for m in _SWIFT_IMPORT_RE.finditer(content)}


def _extract_definitions(content: str) -> tuple[set[str], set[str], set[str]]:
    types: set[str] = set()
    types.update(m.group(1) for m in _SWIFT_CLASS_RE.finditer(content))
    types.update(m.group(1) for m in _SWIFT_STRUCT_RE.finditer(content))
    types.update(m.group(1) for m in _SWIFT_ENUM_RE.finditer(content))
    types.update(m.group(1) for m in _SWIFT_TYPEALIAS_RE.finditer(content))

    protocols = {m.group(1) for m in _SWIFT_PROTOCOL_RE.finditer(content)}
    extensions = {m.group(1) for m in _SWIFT_EXTENSION_RE.finditer(content)}
    funcs = {m.group(1) for m in _SWIFT_FUNC_RE.finditer(content)}

    return funcs, types | protocols, extensions


def _extract_conformances(content: str) -> set[str]:
    conformances: set[str] = set()
    for match in _SWIFT_CONFORMANCE_RE.finditer(content):
        inheritance = match.group(1)
        for part in inheritance.split(","):
            part = part.strip()
            part = re.sub(r"<[^>]{1,200}>", "", part)
            part = re.sub(r"\s{1,20}where\s{1,20}[^\n]{0,300}", "", part)
            if part and part[0].isupper():
                conformances.add(part.strip())
    return conformances


def _extract_references(content: str) -> tuple[set[str], set[str], set[tuple[str, str]]]:
    type_refs = {m.group(1) for m in _SWIFT_TYPE_REF_RE.finditer(content) if m.group(1)[0].isupper()}
    func_calls = {m.group(1) for m in _SWIFT_FUNC_CALL_RE.finditer(content)}
    dot_calls = {(m.group(1), m.group(2)) for m in _SWIFT_DOT_CALL_RE.finditer(content)}
    return type_refs, func_calls, dot_calls


class _SwiftIndex:
    module_to_frags: dict[str, list[FragmentId]]
    type_defs: dict[str, list[FragmentId]]
    func_defs: dict[str, list[FragmentId]]
    extension_targets: dict[str, list[FragmentId]]

    def __init__(self) -> None:
        self.module_to_frags = defaultdict(list)
        self.type_defs = defaultdict(list)
        self.func_defs = defaultdict(list)
        self.extension_targets = defaultdict(list)


class SwiftEdgeBuilder(EdgeBuilder):
    weight = 0.75
    import_weight = 0.70
    conformance_weight = 0.70
    extension_weight = 0.70
    type_weight = 0.65
    func_weight = 0.60
    same_module_weight = 0.50
    reverse_weight_factor = 0.4

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        swift_changed = [f for f in changed_files if _is_swift_file(f)]
        if not swift_changed:
            return []

        changed_set = set(changed_files)
        discovered: set[Path] = set()

        referenced_types = self._collect_referenced_types(swift_changed)
        if referenced_types:
            discovered.update(self._find_files_defining_types(all_candidate_files, changed_set, referenced_types))

        defined_types = self._collect_defined_types(swift_changed)
        if defined_types:
            discovered.update(self._find_files_referencing_types(all_candidate_files, changed_set, defined_types))

        return list(discovered)

    def _collect_referenced_types(self, swift_changed: list[Path]) -> set[str]:
        types: set[str] = set()
        for f in swift_changed:
            try:
                content = f.read_text(encoding="utf-8")
                type_refs, _, _ = _extract_references(content)
                types.update(t.lower() for t in type_refs if len(t) > 2)
            except (OSError, UnicodeDecodeError):
                continue
        return types

    def _find_files_defining_types(self, all_candidate_files: list[Path], changed_set: set[Path], types: set[str]) -> list[Path]:
        discovered: list[Path] = []
        for candidate in all_candidate_files:
            if candidate in changed_set or not _is_swift_file(candidate):
                continue
            if self._defines_any_type(candidate, types):
                discovered.append(candidate)
        return discovered

    def _defines_any_type(self, candidate: Path, types: set[str]) -> bool:
        try:
            content = candidate.read_text(encoding="utf-8")
            _, defined_types, _ = _extract_definitions(content)
            return any(t.lower() in types for t in defined_types)
        except (OSError, UnicodeDecodeError):
            return False

    def _collect_defined_types(self, swift_changed: list[Path]) -> set[str]:
        types: set[str] = set()
        for f in swift_changed:
            try:
                content = f.read_text(encoding="utf-8")
                _, defined_types, _ = _extract_definitions(content)
                types.update(t.lower() for t in defined_types)
            except (OSError, UnicodeDecodeError):
                continue
        return types

    def _find_files_referencing_types(
        self, all_candidate_files: list[Path], changed_set: set[Path], types: set[str]
    ) -> list[Path]:
        discovered: list[Path] = []
        for candidate in all_candidate_files:
            if candidate in changed_set or not _is_swift_file(candidate):
                continue
            if self._references_any_type(candidate, types):
                discovered.append(candidate)
        return discovered

    def _references_any_type(self, candidate: Path, types: set[str]) -> bool:
        try:
            content = candidate.read_text(encoding="utf-8")
            type_refs, _, _ = _extract_references(content)
            return any(t.lower() in types for t in type_refs)
        except (OSError, UnicodeDecodeError):
            return False

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        swift_frags = [f for f in fragments if _is_swift_file(f.path)]
        if not swift_frags:
            return {}

        edges: EdgeDict = {}
        idx = self._build_index(swift_frags)

        for sf in swift_frags:
            self._add_fragment_edges(sf, idx, edges)

        return edges

    def _build_index(self, swift_frags: list[Fragment]) -> _SwiftIndex:
        idx = _SwiftIndex()
        for f in swift_frags:
            self._index_fragment(f, idx)
        return idx

    def _index_fragment(self, f: Fragment, idx: _SwiftIndex) -> None:
        parent = f.path.parent.name.lower()
        idx.module_to_frags[parent].append(f.id)

        funcs, types, extensions = _extract_definitions(f.content)
        for t in types:
            idx.type_defs[t.lower()].append(f.id)
        for fn in funcs:
            idx.func_defs[fn.lower()].append(f.id)
        for ext in extensions:
            idx.extension_targets[ext.lower()].append(f.id)

    def _add_fragment_edges(self, sf: Fragment, idx: _SwiftIndex, edges: EdgeDict) -> None:
        imports = _extract_imports(sf.content)
        type_refs, func_calls, dot_calls = _extract_references(sf.content)
        conformances = _extract_conformances(sf.content)
        _, _, extensions = _extract_definitions(sf.content)

        self._add_import_edges(sf.id, imports, idx, edges)
        self._add_conformance_edges(sf.id, conformances, idx, edges)
        self._add_extension_edges(sf.id, extensions, idx, edges)
        self._add_type_edges(sf.id, type_refs, idx, edges)
        self._add_func_edges(sf.id, func_calls, idx, edges)
        self._add_dot_call_edges(sf.id, dot_calls, idx, edges)
        self._add_same_module_edges(sf, idx, edges)

    def _add_import_edges(self, sf_id: FragmentId, imports: set[str], idx: _SwiftIndex, edges: EdgeDict) -> None:
        for imp in imports:
            for fid in idx.module_to_frags.get(imp.lower(), []):
                if fid != sf_id:
                    self.add_edge(edges, sf_id, fid, self.import_weight)

    def _add_conformance_edges(self, sf_id: FragmentId, conformances: set[str], idx: _SwiftIndex, edges: EdgeDict) -> None:
        for conf in conformances:
            for fid in idx.type_defs.get(conf.lower(), []):
                if fid != sf_id:
                    self.add_edge(edges, sf_id, fid, self.conformance_weight)

    def _add_extension_edges(self, sf_id: FragmentId, extensions: set[str], idx: _SwiftIndex, edges: EdgeDict) -> None:
        for ext in extensions:
            for fid in idx.type_defs.get(ext.lower(), []):
                if fid != sf_id:
                    self.add_edge(edges, sf_id, fid, self.extension_weight)

    def _add_type_edges(self, sf_id: FragmentId, type_refs: set[str], idx: _SwiftIndex, edges: EdgeDict) -> None:
        for type_ref in type_refs:
            for fid in idx.type_defs.get(type_ref.lower(), []):
                if fid != sf_id:
                    self.add_edge(edges, sf_id, fid, self.type_weight)

    def _add_func_edges(self, sf_id: FragmentId, func_calls: set[str], idx: _SwiftIndex, edges: EdgeDict) -> None:
        for func_call in func_calls:
            for fid in idx.func_defs.get(func_call.lower(), []):
                if fid != sf_id:
                    self.add_edge(edges, sf_id, fid, self.func_weight)

    def _add_dot_call_edges(self, sf_id: FragmentId, dot_calls: set[tuple[str, str]], idx: _SwiftIndex, edges: EdgeDict) -> None:
        for obj, _ in dot_calls:
            for fid in idx.type_defs.get(obj.lower(), []):
                if fid != sf_id:
                    self.add_edge(edges, sf_id, fid, self.func_weight)

    def _add_same_module_edges(self, sf: Fragment, idx: _SwiftIndex, edges: EdgeDict) -> None:
        current_module = sf.path.parent.name.lower()
        for fid in idx.module_to_frags.get(current_module, []):
            if fid != sf.id:
                self.add_edge(edges, sf.id, fid, self.same_module_weight)
