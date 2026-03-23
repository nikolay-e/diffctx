from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_R_EXTS = {".r", ".rmd"}

_R_SOURCE_RE = re.compile(r'source\s*\(\s*["\']([^"\']+)["\']\s*\)')
_R_LIBRARY_RE = re.compile(r'(?:library|require)\s*\(\s*["\']?(\w+)["\']?\s*\)')

_FUNC_ASSIGN_RE = re.compile(r"^\s*([a-zA-Z._]\w*)\s*<-\s*function\s*\(", re.MULTILINE)
_FUNC_EQUALS_RE = re.compile(r"^\s*([a-zA-Z._]\w*)\s*=\s*function\s*\(", re.MULTILINE)
_S4_CLASS_RE = re.compile(r'setClass\s*\(\s*["\'](\w+)["\']', re.MULTILINE)
_S4_GENERIC_RE = re.compile(r'setGeneric\s*\(\s*["\'](\w+)["\']', re.MULTILINE)
_S4_METHOD_RE = re.compile(r'setMethod\s*\(\s*["\'](\w+)["\']', re.MULTILINE)
_R5_CLASS_RE = re.compile(r'setRefClass\s*\(\s*["\'](\w+)["\']', re.MULTILINE)
_R6_CLASS_RE = re.compile(r"(\w+)\s*<-\s*R6Class\s*\(", re.MULTILINE)
_S4_CONTAINS_RE = re.compile(r'contains\s*=\s*["\'](\w+)["\']', re.MULTILINE)
_S4_CONTAINS_MULTI_RE = re.compile(r"contains\s*=\s*c\s*\(([^)]+)\)", re.MULTILINE)

_FUNC_CALL_RE = re.compile(r"(?<!\w)([a-zA-Z._]\w{1,100})\s*\(")
_NS_CALL_RE = re.compile(r"(\w+)::(\w+)\s*\(")

_R_KEYWORDS = frozenset(
    {
        "if",
        "else",
        "for",
        "while",
        "repeat",
        "in",
        "next",
        "break",
        "return",
        "function",
        "library",
        "require",
        "source",
        "TRUE",
        "FALSE",
        "NULL",
        "NA",
        "Inf",
        "NaN",
        "c",
        "list",
        "vector",
        "matrix",
        "data.frame",
        "array",
        "print",
        "cat",
        "paste",
        "paste0",
        "sprintf",
        "length",
        "nrow",
        "ncol",
        "dim",
        "names",
        "colnames",
        "rownames",
        "which",
        "any",
        "all",
        "is.null",
        "is.na",
        "is.numeric",
        "class",
        "typeof",
        "str",
        "summary",
        "head",
        "tail",
        "apply",
        "sapply",
        "lapply",
        "tapply",
        "mapply",
        "vapply",
        "do.call",
        "tryCatch",
        "stop",
        "warning",
        "message",
        "seq",
        "rep",
        "sort",
        "order",
        "unique",
        "table",
        "sum",
        "mean",
        "median",
        "max",
        "min",
        "range",
        "var",
        "sd",
        "read.csv",
        "write.csv",
        "readRDS",
        "saveRDS",
        "match.arg",
        "missing",
        "sys.call",
        "on.exit",
    }
)

_DIFF_SOURCE_RE = re.compile(r"""^\+.*source\s*\(\s*['"]([^'"]+)['"]""", re.MULTILINE)
_DIFF_LIBRARY_RE = re.compile(r"""^\+.*(?:library|require)\s*\(\s*['"]?(\w+)['"]?""", re.MULTILINE)


def _is_r_file(path: Path) -> bool:
    return path.suffix.lower() in _R_EXTS


def _extract_sources(content: str) -> set[str]:
    return {m.group(1) for m in _R_SOURCE_RE.finditer(content)}


def _extract_libraries(content: str) -> set[str]:
    return {m.group(1) for m in _R_LIBRARY_RE.finditer(content)}


def _extract_definitions(content: str) -> tuple[set[str], set[str]]:
    funcs: set[str] = set()
    for m in _FUNC_ASSIGN_RE.finditer(content):
        name = m.group(1)
        if name not in _R_KEYWORDS and len(name) >= 2:
            funcs.add(name)
    for m in _FUNC_EQUALS_RE.finditer(content):
        name = m.group(1)
        if name not in _R_KEYWORDS and len(name) >= 2:
            funcs.add(name)
    for m in _S4_GENERIC_RE.finditer(content):
        funcs.add(m.group(1))
    for m in _S4_METHOD_RE.finditer(content):
        funcs.add(m.group(1))

    classes: set[str] = set()
    classes.update(m.group(1) for m in _S4_CLASS_RE.finditer(content))
    classes.update(m.group(1) for m in _R5_CLASS_RE.finditer(content))
    classes.update(m.group(1) for m in _R6_CLASS_RE.finditer(content))

    return funcs, classes


def _extract_inheritance(content: str) -> set[str]:
    parents: set[str] = set()
    for m in _S4_CONTAINS_RE.finditer(content):
        parents.add(m.group(1))
    for m in _S4_CONTAINS_MULTI_RE.finditer(content):
        for part in m.group(1).split(","):
            name = part.strip().strip("'\"").strip()
            if name:
                parents.add(name)
    return parents


def _extract_references(content: str) -> tuple[set[str], set[str]]:
    func_calls = {m.group(1) for m in _FUNC_CALL_RE.finditer(content) if m.group(1) not in _R_KEYWORDS}

    ns_refs: set[str] = set()
    for m in _NS_CALL_RE.finditer(content):
        ns_refs.add(m.group(1))

    return func_calls, ns_refs


def _collect_r_refs(r_files: list[Path]) -> set[str]:
    refs: set[str] = set()
    for rf in r_files:
        try:
            content = rf.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        refs.update(_extract_sources(content))
    return refs


class RLangEdgeBuilder(EdgeBuilder):
    weight = 0.55
    source_weight = EDGE_WEIGHTS["r_source"].forward
    library_weight = EDGE_WEIGHTS["r_library"].forward
    fn_weight = EDGE_WEIGHTS["r_fn"].forward
    s4_weight = EDGE_WEIGHTS["r_s4"].forward
    reverse_weight_factor = EDGE_WEIGHTS["r_source"].reverse_factor

    def needs_from_diff(self, diff_content: str) -> list[str]:
        refs: list[str] = []
        for m in _DIFF_SOURCE_RE.finditer(diff_content):
            refs.append(m.group(1))
        for m in _DIFF_LIBRARY_RE.finditer(diff_content):
            refs.append(m.group(1).lower())
        return refs

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        r_files = [f for f in changed_files if _is_r_file(f)]
        if not r_files:
            return []

        refs = _collect_r_refs(r_files)
        return discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        r_frags = [f for f in fragments if _is_r_file(f.path)]
        if not r_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)
        fn_defs, class_defs = self._build_indices(r_frags)

        for rf in r_frags:
            self._add_fragment_edges(rf, idx, fn_defs, class_defs, edges)

        return edges

    def _build_indices(self, r_frags: list[Fragment]) -> tuple[dict[str, list[FragmentId]], dict[str, list[FragmentId]]]:
        fn_defs: dict[str, list[FragmentId]] = defaultdict(list)
        class_defs: dict[str, list[FragmentId]] = defaultdict(list)

        for f in r_frags:
            funcs, classes = _extract_definitions(f.content)
            for fn in funcs:
                fn_defs[fn.lower()].append(f.id)
            for cls in classes:
                class_defs[cls.lower()].append(f.id)

        return fn_defs, class_defs

    def _add_source_edges(self, rf: Fragment, idx: FragmentIndex, edges: EdgeDict) -> None:
        for src in _extract_sources(rf.content):
            src_name = src.split("/")[-1].lower()
            self.link_by_stem(rf.id, src_name, idx, edges, self.source_weight)
            self.link_by_path_match(rf.id, src, idx, edges, self.source_weight)

    def _add_inheritance_edges(self, rf: Fragment, class_defs: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        for parent in _extract_inheritance(rf.content):
            for fid in class_defs.get(parent.lower(), []):
                if fid != rf.id:
                    self.add_edge(edges, rf.id, fid, self.s4_weight)

    def _add_fn_call_edges(self, rf: Fragment, fn_defs: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        func_calls, _ns_refs = _extract_references(rf.content)
        self_funcs, _self_classes = _extract_definitions(rf.content)
        self_fn_lower = {fn.lower() for fn in self_funcs}

        for func_call in func_calls:
            func_lower = func_call.lower()
            if func_lower in self_fn_lower:
                continue
            for fid in fn_defs.get(func_lower, []):
                if fid != rf.id:
                    self.add_edge(edges, rf.id, fid, self.fn_weight)

    def _add_fragment_edges(
        self,
        rf: Fragment,
        idx: FragmentIndex,
        fn_defs: dict[str, list[FragmentId]],
        class_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        self._add_source_edges(rf, idx, edges)

        for lib in _extract_libraries(rf.content):
            self.link_by_stem(rf.id, lib.lower(), idx, edges, self.library_weight)

        self._add_inheritance_edges(rf, class_defs, edges)
        self._add_fn_call_edges(rf, fn_defs, edges)
