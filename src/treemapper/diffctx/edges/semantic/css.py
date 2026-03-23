from __future__ import annotations

import re
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_SCSS = ".scss"
_LESS = ".less"
_SASS = ".sass"
_CSS_EXTS = frozenset({".css", _SCSS, _LESS, _SASS})

_CSS_IMPORT_URL_RE = re.compile(
    r"""^\s*@import\s+url\(\s*['"]?([^'")]{1,300})['"]?\s*\)""",
    re.MULTILINE,
)
_CSS_IMPORT_STR_RE = re.compile(
    r"""^\s*@import\s+['"]([^'"]{1,300})['"]""",
    re.MULTILINE,
)
_SCSS_USE_RE = re.compile(r"^\s*@use\s+['\"]([^'\"]{1,300})['\"]", re.MULTILINE)
_SCSS_FORWARD_RE = re.compile(r"^\s*@forward\s+['\"]([^'\"]{1,300})['\"]", re.MULTILINE)
_CSS_COMPOSES_RE = re.compile(r"composes\s*:\s*[^;\n]{0,300}\bfrom\s+['\"]([^'\"]{1,300})['\"]", re.MULTILINE)


def _is_css_file(path: Path) -> bool:
    return path.suffix.lower() in _CSS_EXTS


def _extract_refs(content: str) -> set[str]:
    refs: set[str] = set()

    for m in _CSS_IMPORT_URL_RE.finditer(content):
        refs.add(m.group(1))

    for m in _CSS_IMPORT_STR_RE.finditer(content):
        refs.add(m.group(1))

    for m in _SCSS_USE_RE.finditer(content):
        refs.add(m.group(1))

    for m in _SCSS_FORWARD_RE.finditer(content):
        refs.add(m.group(1))

    for m in _CSS_COMPOSES_RE.finditer(content):
        refs.add(m.group(1))

    return refs


_PARTIAL_EXTS = (_SCSS, _SASS, _LESS, ".css")


def _resolve_partial_candidates(ref: str) -> list[str]:
    candidates = [ref]
    base = ref.rsplit("/", 1)
    if len(base) == 2:
        directory, name = base
        if not name.startswith("_"):
            for ext in _PARTIAL_EXTS:
                candidates.append(f"{directory}/_{name}{ext}")
                candidates.append(f"{directory}/_{name}")
            candidates.append(f"{directory}/_index")
    else:
        name = base[0]
        if not name.startswith("_"):
            for ext in _PARTIAL_EXTS:
                candidates.append(f"_{name}{ext}")
                candidates.append(f"_{name}")
    return candidates


def _ref_to_filename(ref: str) -> str:
    name = ref.split("/")[-1].lower()
    for ext in _PARTIAL_EXTS:
        if name.endswith(ext):
            return name
    return name


class CssEdgeBuilder(EdgeBuilder):
    weight = 0.60
    import_weight = EDGE_WEIGHTS["css_import"].forward
    reverse_weight_factor = EDGE_WEIGHTS["css_import"].reverse_factor

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        css_changed = [f for f in changed_files if _is_css_file(f)]
        if not css_changed:
            return []

        refs: set[str] = set()
        for f in css_changed:
            try:
                content = f.read_text(encoding="utf-8")
                for r in _extract_refs(content):
                    refs.add(_ref_to_filename(r))
                    for candidate in _resolve_partial_candidates(r):
                        refs.add(_ref_to_filename(candidate))
            except (OSError, UnicodeDecodeError):
                continue

        return discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        css_frags = [f for f in fragments if _is_css_file(f.path)]
        if not css_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)

        for cf in css_frags:
            self._add_fragment_edges(cf, idx, edges)

        return edges

    def _add_fragment_edges(self, cf: Fragment, idx: FragmentIndex, edges: EdgeDict) -> None:
        for ref in _extract_refs(cf.content):
            self._link_ref(cf.id, ref, idx, edges)

    def _try_link_candidate(
        self,
        src_id: FragmentId,
        filename: str,
        idx: FragmentIndex,
        edges: EdgeDict,
    ) -> bool:
        for name, frag_ids in idx.by_name.items():
            if name == filename or _strip_css_ext(name) == _strip_css_ext(filename):
                for fid in frag_ids:
                    if fid != src_id:
                        self.add_edge(edges, src_id, fid, self.import_weight)
                        return True
        return False

    def _link_ref(
        self,
        src_id: FragmentId,
        ref: str,
        idx: FragmentIndex,
        edges: EdgeDict,
    ) -> None:
        for candidate in _resolve_partial_candidates(ref):
            if self._try_link_candidate(src_id, _ref_to_filename(candidate), idx, edges):
                return

        self.link_by_path_match(src_id, ref, idx, edges, self.import_weight)


def _strip_css_ext(name: str) -> str:
    for ext in _PARTIAL_EXTS:
        if name.endswith(ext):
            return name[: -len(ext)]
    return name
