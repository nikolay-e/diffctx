from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict

_PHP_EXTS = {".php", ".phtml", ".php3", ".php4", ".php5", ".php7", ".phps"}

_PHP_USE_RE = re.compile(r"^\s*use\s+([A-Z][a-zA-Z0-9_\\]*(?:\s+as\s+\w+)?)\s*;", re.MULTILINE)
_PHP_NAMESPACE_RE = re.compile(r"^\s*namespace\s+([A-Z][a-zA-Z0-9_\\]*)\s*;", re.MULTILINE)
_PHP_REQUIRE_RE = re.compile(r"^\s*(?:require|require_once|include|include_once)\s*\(?['\"]([^'\"]+)['\"]", re.MULTILINE)

_PHP_CLASS_RE = re.compile(r"^\s*(?:abstract\s+)?(?:final\s+)?class\s+([A-Z]\w*)", re.MULTILINE)
_PHP_INTERFACE_RE = re.compile(r"^\s*interface\s+([A-Z]\w*)", re.MULTILINE)
_PHP_TRAIT_RE = re.compile(r"^\s*trait\s+([A-Z]\w*)", re.MULTILINE)
_PHP_ENUM_RE = re.compile(r"^\s*enum\s+([A-Z]\w*)", re.MULTILINE)
_PHP_FUNCTION_RE = re.compile(r"^\s*(?:public |private |protected )?(?:static )?function\s+([a-z_]\w*)", re.MULTILINE)

_PHP_EXTENDS_RE = re.compile(r"class\s+\w+\s+extends\s+([A-Z]\w*)")
_PHP_IMPLEMENTS_RE = re.compile(r"implements\s+([A-Z]\w*(?:\s*,\s*[A-Z]\w*)*)")
_PHP_USE_TRAIT_RE = re.compile(r"^\s*use\s+([A-Z]\w*(?:\s*,\s*[A-Z]\w*)*)\s*;", re.MULTILINE)

_PHP_TYPE_HINT_RE = re.compile(r"(?::|->)\s*([A-Z]\w*)\b")
_PHP_NEW_RE = re.compile(r"new\s+([A-Z]\w*)")
_PHP_STATIC_CALL_RE = re.compile(r"([A-Z]\w*)::(?!\$)")
_PHP_INSTANCEOF_RE = re.compile(r"instanceof\s+([A-Z]\w*)")


def _is_php_file(path: Path) -> bool:
    return path.suffix.lower() in _PHP_EXTS


def _extract_uses(content: str) -> set[str]:
    uses: set[str] = set()
    for m in _PHP_USE_RE.finditer(content):
        use = m.group(1).split(" as ")[0].strip()
        if "\\" in use:
            parts = use.split("\\")
            uses.add(parts[-1])
            uses.add(use)
        else:
            uses.add(use)
    return uses


def _extract_namespace(content: str) -> str | None:
    match = _PHP_NAMESPACE_RE.search(content)
    return match.group(1) if match else None


def _extract_requires(content: str) -> set[str]:
    return {m.group(1) for m in _PHP_REQUIRE_RE.finditer(content)}


def _extract_definitions(content: str) -> tuple[set[str], set[str], set[str], set[str]]:
    classes = {m.group(1) for m in _PHP_CLASS_RE.finditer(content)}
    interfaces = {m.group(1) for m in _PHP_INTERFACE_RE.finditer(content)}
    traits = {m.group(1) for m in _PHP_TRAIT_RE.finditer(content)}
    enums = {m.group(1) for m in _PHP_ENUM_RE.finditer(content)}
    return classes, interfaces, traits, enums


def _extract_inheritance(content: str) -> set[str]:
    refs: set[str] = set()
    for m in _PHP_EXTENDS_RE.finditer(content):
        refs.add(m.group(1))
    for m in _PHP_IMPLEMENTS_RE.finditer(content):
        for cls in m.group(1).split(","):
            refs.add(cls.strip())
    for m in _PHP_USE_TRAIT_RE.finditer(content):
        for trait in m.group(1).split(","):
            refs.add(trait.strip())
    return refs


def _extract_type_refs(content: str) -> set[str]:
    refs: set[str] = set()
    refs.update(m.group(1) for m in _PHP_TYPE_HINT_RE.finditer(content))
    refs.update(m.group(1) for m in _PHP_NEW_RE.finditer(content))
    refs.update(m.group(1) for m in _PHP_STATIC_CALL_RE.finditer(content))
    refs.update(m.group(1) for m in _PHP_INSTANCEOF_RE.finditer(content))
    return refs


class _PHPIndex:
    name_to_frags: dict[str, list[FragmentId]]
    path_to_frags: dict[str, list[FragmentId]]
    namespace_to_frags: dict[str, list[FragmentId]]
    class_to_frags: dict[str, list[FragmentId]]
    fqn_to_frags: dict[str, list[FragmentId]]

    def __init__(self) -> None:
        self.name_to_frags = defaultdict(list)
        self.path_to_frags = defaultdict(list)
        self.namespace_to_frags = defaultdict(list)
        self.class_to_frags = defaultdict(list)
        self.fqn_to_frags = defaultdict(list)


class PHPEdgeBuilder(EdgeBuilder):
    weight = 0.70
    use_weight = 0.75
    require_weight = 0.70
    inheritance_weight = 0.80
    type_weight = 0.60
    same_namespace_weight = 0.55
    reverse_weight_factor = 0.4

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        php_frags = [f for f in fragments if _is_php_file(f.path)]
        if not php_frags:
            return {}

        edges: EdgeDict = {}
        idx = self._build_index(php_frags, repo_root)

        for pf in php_frags:
            self._add_fragment_edges(pf, idx, edges)

        return edges

    def _build_index(self, php_frags: list[Fragment], repo_root: Path | None) -> _PHPIndex:
        idx = _PHPIndex()
        for f in php_frags:
            self._index_fragment(f, idx, repo_root)
        return idx

    def _index_fragment(self, f: Fragment, idx: _PHPIndex, repo_root: Path | None) -> None:
        stem = f.path.stem.lower()
        idx.name_to_frags[stem].append(f.id)

        self._index_paths(f, idx, repo_root)
        self._index_namespace(f, idx)
        self._index_types(f, idx)

    def _index_paths(self, f: Fragment, idx: _PHPIndex, repo_root: Path | None) -> None:
        self.index_paths_for_fragment(f, idx.path_to_frags, repo_root)

    def _index_namespace(self, f: Fragment, idx: _PHPIndex) -> None:
        ns = _extract_namespace(f.content)
        if not ns:
            return
        idx.namespace_to_frags[ns.lower()].append(f.id)
        ns_parts = ns.split("\\")
        for i in range(len(ns_parts)):
            partial_ns = "\\".join(ns_parts[: i + 1])
            idx.namespace_to_frags[partial_ns.lower()].append(f.id)

    def _index_types(self, f: Fragment, idx: _PHPIndex) -> None:
        ns = _extract_namespace(f.content)
        classes, interfaces, traits, enums = _extract_definitions(f.content)
        all_types = classes | interfaces | traits | enums
        for t in all_types:
            idx.class_to_frags[t.lower()].append(f.id)
            if ns:
                fqn = f"{ns}\\{t}"
                idx.fqn_to_frags[fqn.lower()].append(f.id)

    def _add_fragment_edges(self, pf: Fragment, idx: _PHPIndex, edges: EdgeDict) -> None:
        uses = _extract_uses(pf.content)
        requires = _extract_requires(pf.content)
        inheritance = _extract_inheritance(pf.content)
        type_refs = _extract_type_refs(pf.content)
        current_ns = _extract_namespace(pf.content)

        self._add_use_edges(pf.id, uses, idx, edges)
        self._add_require_edges(pf.id, requires, idx, edges)
        self._add_inheritance_edges(pf.id, inheritance, idx, edges)
        self._add_type_edges(pf.id, type_refs, idx, edges)
        self._add_namespace_edges(pf.id, current_ns, idx, edges)

    def _add_use_edges(self, pf_id: FragmentId, uses: set[str], idx: _PHPIndex, edges: EdgeDict) -> None:
        for use in uses:
            use_lower = use.lower().replace("\\", "\\\\")
            for fid in idx.fqn_to_frags.get(use_lower, []):
                if fid != pf_id:
                    self.add_edge(edges, pf_id, fid, self.use_weight)

            class_name = use.split("\\")[-1].lower()
            for fid in idx.class_to_frags.get(class_name, []):
                if fid != pf_id:
                    self.add_edge(edges, pf_id, fid, self.use_weight)

    def _add_require_edges(self, pf_id: FragmentId, requires: set[str], idx: _PHPIndex, edges: EdgeDict) -> None:
        for req in requires:
            self._link_require_by_name(pf_id, req, idx, edges)
            self._link_require_by_path(pf_id, req, idx, edges)

    def _link_require_by_name(self, pf_id: FragmentId, req: str, idx: _PHPIndex, edges: EdgeDict) -> None:
        req_name = req.split("/")[-1].replace(".php", "").lower()
        self.add_edges_from_ids(pf_id, idx.name_to_frags.get(req_name, []), self.require_weight, edges)

    def _link_require_by_path(self, pf_id: FragmentId, req: str, idx: _PHPIndex, edges: EdgeDict) -> None:
        for path_str, frag_ids in idx.path_to_frags.items():
            if req in path_str:
                self.add_edges_from_ids(pf_id, frag_ids, self.require_weight, edges)

    def _add_inheritance_edges(self, pf_id: FragmentId, inheritance: set[str], idx: _PHPIndex, edges: EdgeDict) -> None:
        for parent in inheritance:
            for fid in idx.class_to_frags.get(parent.lower(), []):
                if fid != pf_id:
                    self.add_edge(edges, pf_id, fid, self.inheritance_weight)

    def _add_type_edges(self, pf_id: FragmentId, type_refs: set[str], idx: _PHPIndex, edges: EdgeDict) -> None:
        for type_ref in type_refs:
            for fid in idx.class_to_frags.get(type_ref.lower(), []):
                if fid != pf_id:
                    self.add_edge(edges, pf_id, fid, self.type_weight)

    def _add_namespace_edges(self, pf_id: FragmentId, current_ns: str | None, idx: _PHPIndex, edges: EdgeDict) -> None:
        if not current_ns:
            return
        for fid in idx.namespace_to_frags.get(current_ns.lower(), []):
            if fid != pf_id:
                self.add_edge(edges, pf_id, fid, self.same_namespace_weight)
