from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict

_RUBY_EXTS = {".rb", ".rake", ".gemspec"}
_RUBY_FILES = {"rakefile", "gemfile", "guardfile", "vagrantfile", "capfile", "podfile"}

_RUBY_REQUIRE_RE = re.compile(r"^\s*require(?:_relative)?[\s(]+['\"]([^'\"]+)['\"]", re.MULTILINE)
_RUBY_REQUIRE_RELATIVE_RE = re.compile(r"^\s*require_relative[\s(]+['\"]([^'\"]+)['\"]", re.MULTILINE)
_RUBY_LOAD_RE = re.compile(r"^\s*load[\s(]+['\"]([^'\"]+)['\"]", re.MULTILINE)
_RUBY_AUTOLOAD_RE = re.compile(r"^\s*autoload[\s(]+:(\w+),\s*['\"]([^'\"]+)['\"]", re.MULTILINE)

_RUBY_CLASS_RE = re.compile(r"^\s*class\s+([A-Z]\w*(?:::[A-Z]\w*)*)", re.MULTILINE)
_RUBY_MODULE_RE = re.compile(r"^\s*module\s+([A-Z]\w*(?:::[A-Z]\w*)*)", re.MULTILINE)
_RUBY_DEF_RE = re.compile(r"^\s*def\s+(?:self\.)?([a-z_]\w*)", re.MULTILINE)
_RUBY_INCLUDE_RE = re.compile(r"^\s*(?:include|extend|prepend)\s+([A-Z]\w*(?:::[A-Z]\w*)*)", re.MULTILINE)
_RUBY_INHERIT_RE = re.compile(r"class\s+\w+\s*<\s*([A-Z]\w*(?:::[A-Z]\w*)*)")

_RUBY_CONST_REF_RE = re.compile(r"(?<![a-z_])([A-Z]\w*(?:::[A-Z]\w*)*)")
_RUBY_METHOD_CALL_RE = re.compile(r"\.([a-z_]\w*)\s*(?:$|[^a-z_])")


def _is_ruby_file(path: Path) -> bool:
    if path.suffix.lower() in _RUBY_EXTS:
        return True
    return path.name.lower() in _RUBY_FILES


def _extract_requires(content: str) -> tuple[set[str], set[str]]:
    requires: set[str] = set()
    relative_requires: set[str] = set()

    for m in _RUBY_REQUIRE_RE.finditer(content):
        req = m.group(1)
        if "require_relative" not in m.group(0):
            requires.add(req)

    for m in _RUBY_REQUIRE_RELATIVE_RE.finditer(content):
        relative_requires.add(m.group(1))

    for m in _RUBY_LOAD_RE.finditer(content):
        requires.add(m.group(1))

    return requires, relative_requires


def _extract_autoloads(content: str) -> dict[str, str]:
    return {m.group(1): m.group(2) for m in _RUBY_AUTOLOAD_RE.finditer(content)}


def _extract_definitions(content: str) -> tuple[set[str], set[str], set[str]]:
    classes = {m.group(1).split("::")[-1] for m in _RUBY_CLASS_RE.finditer(content)}
    modules = {m.group(1).split("::")[-1] for m in _RUBY_MODULE_RE.finditer(content)}
    methods = {m.group(1) for m in _RUBY_DEF_RE.finditer(content)}
    return classes, modules, methods


def _extract_includes(content: str) -> set[str]:
    includes: set[str] = set()
    for m in _RUBY_INCLUDE_RE.finditer(content):
        full = m.group(1)
        includes.add(full.split("::")[-1])
    for m in _RUBY_INHERIT_RE.finditer(content):
        full = m.group(1)
        includes.add(full.split("::")[-1])
    return includes


def _extract_const_refs(content: str) -> set[str]:
    refs: set[str] = set()
    for m in _RUBY_CONST_REF_RE.finditer(content):
        full = m.group(1)
        refs.add(full.split("::")[-1])
    return refs


def _underscore_to_camel(name: str) -> str:
    return "".join(part.capitalize() for part in name.split("_"))


class _RubyIndex:
    name_to_frags: dict[str, list[FragmentId]]
    path_to_frags: dict[str, list[FragmentId]]
    class_to_frags: dict[str, list[FragmentId]]
    module_to_frags: dict[str, list[FragmentId]]
    dir_to_frags: dict[Path, list[FragmentId]]

    def __init__(self) -> None:
        self.name_to_frags = defaultdict(list)
        self.path_to_frags = defaultdict(list)
        self.class_to_frags = defaultdict(list)
        self.module_to_frags = defaultdict(list)
        self.dir_to_frags = defaultdict(list)


class RubyEdgeBuilder(EdgeBuilder):
    weight = 0.70
    require_weight = 0.75
    include_weight = 0.70
    const_weight = 0.60
    same_dir_weight = 0.50
    reverse_weight_factor = 0.4

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        ruby_frags = [f for f in fragments if _is_ruby_file(f.path)]
        if not ruby_frags:
            return {}

        edges: EdgeDict = {}
        idx = self._build_index(ruby_frags, repo_root)

        for rf in ruby_frags:
            self._add_fragment_edges(rf, idx, edges, repo_root)

        return edges

    def _build_index(self, ruby_frags: list[Fragment], repo_root: Path | None) -> _RubyIndex:
        idx = _RubyIndex()
        for f in ruby_frags:
            self._index_fragment(f, idx, repo_root)
        return idx

    def _index_fragment(self, f: Fragment, idx: _RubyIndex, repo_root: Path | None) -> None:
        stem = f.path.stem.lower()
        idx.name_to_frags[stem].append(f.id)
        idx.name_to_frags[_underscore_to_camel(stem).lower()].append(f.id)
        idx.dir_to_frags[f.path.parent].append(f.id)

        self._index_paths(f, idx, repo_root)
        self._index_definitions(f, idx)

    def _index_paths(self, f: Fragment, idx: _RubyIndex, repo_root: Path | None) -> None:
        self.index_paths_for_fragment(f, idx.path_to_frags, repo_root)
        if repo_root:
            try:
                rel = f.path.relative_to(repo_root)
                idx.path_to_frags[str(rel.with_suffix(""))].append(f.id)
            except ValueError:
                pass

    def _index_definitions(self, f: Fragment, idx: _RubyIndex) -> None:
        classes, modules, _ = _extract_definitions(f.content)
        for cls in classes:
            idx.class_to_frags[cls.lower()].append(f.id)
        for mod in modules:
            idx.module_to_frags[mod.lower()].append(f.id)

    def _add_fragment_edges(self, rf: Fragment, idx: _RubyIndex, edges: EdgeDict, repo_root: Path | None) -> None:
        requires, relative_requires = _extract_requires(rf.content)
        includes = _extract_includes(rf.content)
        const_refs = _extract_const_refs(rf.content)

        self._add_require_edges(rf.id, requires, idx, edges)
        self._add_relative_require_edges(rf, relative_requires, idx, edges, repo_root)
        self._add_include_edges(rf.id, includes, idx, edges)
        self._add_const_edges(rf.id, const_refs, idx, edges)
        self._add_same_dir_edges(rf, idx, edges)

    def _add_require_edges(self, rf_id: FragmentId, requires: set[str], idx: _RubyIndex, edges: EdgeDict) -> None:
        for req in requires:
            self._link_require_by_name(rf_id, req, idx, edges)
            self._link_require_by_path(rf_id, req, idx, edges)

    def _link_require_by_name(self, rf_id: FragmentId, req: str, idx: _RubyIndex, edges: EdgeDict) -> None:
        req_name = req.split("/")[-1].lower()
        self.add_edges_from_ids(rf_id, idx.name_to_frags.get(req_name, []), self.require_weight, edges)

    def _link_require_by_path(self, rf_id: FragmentId, req: str, idx: _RubyIndex, edges: EdgeDict) -> None:
        for path_str, frag_ids in idx.path_to_frags.items():
            if req in path_str or req.replace("/", "\\") in path_str:
                self.add_edges_from_ids(rf_id, frag_ids, self.require_weight, edges)

    def _add_relative_require_edges(
        self,
        rf: Fragment,
        relative_requires: set[str],
        idx: _RubyIndex,
        edges: EdgeDict,
        repo_root: Path | None,
    ) -> None:
        for rel_req in relative_requires:
            self._link_relative_by_path(rf, rel_req, idx, edges, repo_root)
            self._link_relative_by_name(rf.id, rel_req, idx, edges)

    def _link_relative_by_path(
        self,
        rf: Fragment,
        rel_req: str,
        idx: _RubyIndex,
        edges: EdgeDict,
        repo_root: Path | None,
    ) -> None:
        if not repo_root:
            return
        try:
            target = (rf.path.parent / rel_req).resolve()
            if not target.suffix:
                target = target.with_suffix(".rb")
            target_rel = str(target.relative_to(repo_root))
            for fid in idx.path_to_frags.get(target_rel, []):
                if fid != rf.id:
                    self.add_edge(edges, rf.id, fid, self.require_weight)
        except (ValueError, OSError):
            pass

    def _link_relative_by_name(self, rf_id: FragmentId, rel_req: str, idx: _RubyIndex, edges: EdgeDict) -> None:
        rel_name = rel_req.split("/")[-1].lower()
        for fid in idx.name_to_frags.get(rel_name, []):
            if fid != rf_id:
                self.add_edge(edges, rf_id, fid, self.require_weight)

    def _add_include_edges(self, rf_id: FragmentId, includes: set[str], idx: _RubyIndex, edges: EdgeDict) -> None:
        for inc in includes:
            inc_lower = inc.lower()
            for fid in idx.class_to_frags.get(inc_lower, []):
                if fid != rf_id:
                    self.add_edge(edges, rf_id, fid, self.include_weight)
            for fid in idx.module_to_frags.get(inc_lower, []):
                if fid != rf_id:
                    self.add_edge(edges, rf_id, fid, self.include_weight)

    def _add_const_edges(self, rf_id: FragmentId, const_refs: set[str], idx: _RubyIndex, edges: EdgeDict) -> None:
        for const in const_refs:
            const_lower = const.lower()
            for fid in idx.class_to_frags.get(const_lower, []):
                if fid != rf_id:
                    self.add_edge(edges, rf_id, fid, self.const_weight)
            for fid in idx.module_to_frags.get(const_lower, []):
                if fid != rf_id:
                    self.add_edge(edges, rf_id, fid, self.const_weight)

    def _add_same_dir_edges(self, rf: Fragment, idx: _RubyIndex, edges: EdgeDict) -> None:
        for fid in idx.dir_to_frags.get(rf.path.parent, []):
            if fid != rf.id:
                self.add_edge(edges, rf.id, fid, self.same_dir_weight)
