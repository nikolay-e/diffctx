from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict

_C_EXTENSIONS = {".c", ".h"}
_CPP_EXTENSIONS = {".cpp", ".hpp", ".cc", ".cxx", ".hxx", ".c++", ".h++", ".hh", ".ipp", ".tpp"}
_OBJC_EXTENSIONS = {".m", ".mm"}
_ALL_C_FAMILY = _C_EXTENSIONS | _CPP_EXTENSIONS | _OBJC_EXTENSIONS

_INCLUDE_RE = re.compile(r'^\s*#\s*(?:include|import)\s*[<"]([^>"]+)[>"]', re.MULTILINE)

_FUNC_DEF_RE = re.compile(
    r"^\s*(?:static\s+|inline\s+|virtual\s+|explicit\s+|constexpr\s+)*"
    r"(?:[\w:]+\s+)+"
    r"(\w+)\s*\([^)]*\)\s*(?:const\s*)?(?:override\s*)?(?:final\s*)?"
    r"(?:noexcept(?:\([^)]*\))?\s*)?"
    r"\s*\{",
    re.MULTILINE,
)

_CLASS_RE = re.compile(r"^\s{0,20}(?:template\s{0,5}<[^>]{0,200}>\s{0,5})?(?:class|struct)\s+(\w+)", re.MULTILINE)
_TYPEDEF_RE = re.compile(r"^\s{0,20}typedef\s{1,10}[^\n;]{1,500}\s{1,10}(\w+)\s{0,10};", re.MULTILINE)
_USING_TYPE_RE = re.compile(r"^\s{0,20}using\s+(\w+)\s{0,10}=", re.MULTILINE)
_ENUM_RE = re.compile(r"^\s*enum\s+(?:class\s+)?(\w+)", re.MULTILINE)
_NAMESPACE_RE = re.compile(r"^\s*namespace\s+(\w+)", re.MULTILINE)

_FUNC_CALL_RE = re.compile(r"\b(\w+)\s*\(")
_TYPE_REF_RE = re.compile(r"\b([A-Z]\w*)\b")

_METHOD_IMPL_RE = re.compile(r"^\s*(?:[\w:]+\s+)?(\w+)::(\w+)\s*\(", re.MULTILINE)


def _is_c_family(path: Path) -> bool:
    return path.suffix.lower() in _ALL_C_FAMILY


def _extract_includes(content: str) -> set[str]:
    includes: set[str] = set()
    for match in _INCLUDE_RE.finditer(content):
        header = match.group(1)
        includes.add(header)
        if "/" in header:
            includes.add(header.split("/")[-1])
    return includes


def _extract_definitions(content: str) -> tuple[set[str], set[str], set[str]]:
    functions: set[str] = set()
    types: set[str] = set()
    namespaces: set[str] = set()

    for match in _FUNC_DEF_RE.finditer(content):
        functions.add(match.group(1))

    for pattern in [_CLASS_RE, _TYPEDEF_RE, _USING_TYPE_RE, _ENUM_RE]:
        for match in pattern.finditer(content):
            types.add(match.group(1))

    for match in _NAMESPACE_RE.finditer(content):
        namespaces.add(match.group(1))

    for match in _METHOD_IMPL_RE.finditer(content):
        class_name, method_name = match.groups()
        types.add(class_name)
        functions.add(method_name)

    return functions, types, namespaces


def _extract_references(content: str, own_defs: set[str]) -> tuple[set[str], set[str]]:
    calls: set[str] = set()
    type_refs: set[str] = set()

    for match in _FUNC_CALL_RE.finditer(content):
        name = match.group(1)
        if name not in own_defs and not name.startswith("_") and len(name) > 2:
            calls.add(name)

    for match in _TYPE_REF_RE.finditer(content):
        name = match.group(1)
        if name not in own_defs and len(name) > 2:
            type_refs.add(name)

    return calls, type_refs


class _CIndex:
    header_to_frags: dict[str, list[FragmentId]]
    func_defs: dict[str, list[FragmentId]]
    type_defs: dict[str, list[FragmentId]]
    frag_own_defs: dict[FragmentId, set[str]]

    def __init__(self) -> None:
        self.header_to_frags = defaultdict(list)
        self.func_defs = defaultdict(list)
        self.type_defs = defaultdict(list)
        self.frag_own_defs = {}


class CFamilyEdgeBuilder(EdgeBuilder):
    weight = 0.70
    include_weight = 0.65
    call_weight = 0.55
    type_weight = 0.50
    reverse_weight_factor = 0.40

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        c_changed = [f for f in changed_files if _is_c_family(f)]
        if not c_changed:
            return []

        changed_set = set(changed_files)
        discovered: set[Path] = set()

        included_headers = self._collect_included_headers(c_changed)
        if included_headers:
            discovered.update(self._find_files_for_headers(all_candidate_files, changed_set, included_headers))

        changed_names = self._collect_changed_names(c_changed)
        if changed_names:
            discovered.update(self._find_files_including_headers(all_candidate_files, changed_set, changed_names))

        return list(discovered)

    def _collect_included_headers(self, c_changed: list[Path]) -> set[str]:
        included: set[str] = set()
        for f in c_changed:
            try:
                content = f.read_text(encoding="utf-8")
                for inc in _extract_includes(content):
                    included.add(inc)
                    if "/" in inc:
                        included.add(inc.split("/")[-1])
            except (OSError, UnicodeDecodeError):
                continue
        return included

    def _find_files_for_headers(self, all_candidate_files: list[Path], changed_set: set[Path], headers: set[str]) -> list[Path]:
        discovered: list[Path] = []
        for candidate in all_candidate_files:
            if candidate in changed_set or not _is_c_family(candidate):
                continue
            if candidate.name in headers:
                discovered.append(candidate)
        return discovered

    def _collect_changed_names(self, c_changed: list[Path]) -> set[str]:
        names: set[str] = set()
        for f in c_changed:
            names.add(f.name)
            names.add(f.stem + ".h")
            names.add(f.stem + ".hpp")
        return names

    def _find_files_including_headers(
        self, all_candidate_files: list[Path], changed_set: set[Path], changed_names: set[str]
    ) -> list[Path]:
        discovered: list[Path] = []
        for candidate in all_candidate_files:
            if candidate in changed_set or not _is_c_family(candidate):
                continue
            if self._includes_any_header(candidate, changed_names):
                discovered.append(candidate)
        return discovered

    def _includes_any_header(self, candidate: Path, changed_names: set[str]) -> bool:
        try:
            content = candidate.read_text(encoding="utf-8")
            includes = _extract_includes(content)
            for inc in includes:
                inc_name = inc.split("/")[-1] if "/" in inc else inc
                if inc_name in changed_names:
                    return True
            return False
        except (OSError, UnicodeDecodeError):
            return False

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        c_frags = [f for f in fragments if _is_c_family(f.path)]
        if not c_frags:
            return {}

        edges: EdgeDict = {}
        idx = self._build_index(c_frags)

        for f in c_frags:
            self._add_fragment_edges(f, idx, edges)

        self._link_header_impl_pairs(c_frags, edges)
        return edges

    def _build_index(self, c_frags: list[Fragment]) -> _CIndex:
        idx = _CIndex()

        for f in c_frags:
            idx.header_to_frags[f.path.name].append(f.id)
            if f.path.stem:
                idx.header_to_frags[f.path.stem + ".h"].append(f.id)
                idx.header_to_frags[f.path.stem + ".hpp"].append(f.id)

            functions, types, _ = _extract_definitions(f.content)
            idx.frag_own_defs[f.id] = functions | types

            for func in functions:
                idx.func_defs[func].append(f.id)
            for t in types:
                idx.type_defs[t].append(f.id)

        return idx

    def _add_fragment_edges(self, f: Fragment, idx: _CIndex, edges: EdgeDict) -> None:
        self._add_include_edges(f, idx.header_to_frags, edges)

        own_defs = idx.frag_own_defs.get(f.id, set())
        calls, type_refs = _extract_references(f.content, own_defs)

        self._add_call_edges(f.id, calls, idx.func_defs, edges)
        self._add_type_edges(f.id, type_refs, idx.type_defs, edges)

    def _add_include_edges(self, f: Fragment, header_to_frags: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        for inc in _extract_includes(f.content):
            inc_name = inc.split("/")[-1] if "/" in inc else inc
            for target_id in header_to_frags.get(inc_name, []):
                if target_id != f.id:
                    self.add_edge(edges, f.id, target_id, self.include_weight)

    def _add_call_edges(
        self,
        src_id: FragmentId,
        calls: set[str],
        func_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for call in calls:
            for def_id in func_defs.get(call, []):
                if def_id != src_id:
                    self.add_edge(edges, src_id, def_id, self.call_weight)

    def _add_type_edges(
        self,
        src_id: FragmentId,
        type_refs: set[str],
        type_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for t in type_refs:
            for def_id in type_defs.get(t, []):
                if def_id != src_id:
                    self.add_edge(edges, src_id, def_id, self.type_weight)

    def _link_header_impl_pairs(self, frags: list[Fragment], edges: EdgeDict) -> None:
        by_stem: dict[str, list[Fragment]] = defaultdict(list)
        for f in frags:
            by_stem[f.path.stem.lower()].append(f)

        for _stem, group in by_stem.items():
            if len(group) < 2:
                continue

            headers = [f for f in group if f.path.suffix.lower() in {".h", ".hpp", ".hh", ".hxx", ".h++"}]
            impls = [f for f in group if f.path.suffix.lower() in {".c", ".cpp", ".cc", ".cxx", ".c++", ".m", ".mm"}]

            for h in headers:
                for impl in impls:
                    self.add_edge(edges, h.id, impl.id, self.weight)
