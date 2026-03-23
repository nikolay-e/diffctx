from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_NIM_EXTS = {".nim", ".nims"}

_NIM_IMPORT_RE = re.compile(r"^\s*import\s+([\w/]+)", re.MULTILINE)
_NIM_FROM_IMPORT_RE = re.compile(r"^\s*from\s+([\w/]+)\s+import", re.MULTILINE)
_NIM_INCLUDE_RE = re.compile(r"^\s*include\s+([\w/]+)", re.MULTILINE)

_PROC_RE = re.compile(
    r"^\s*(?:proc|func|method|iterator|converter|template|macro)\s+([a-zA-Z_]\w*)\s*(?:\*\s*)?[(\[]",
    re.MULTILINE,
)
_TYPE_DEF_RE = re.compile(
    r"^\s*([A-Z]\w*)\s*(?:\*\s*)?=\s*(?:object|ref\s+object|enum|tuple|concept|distinct)",
    re.MULTILINE,
)
_TYPE_IN_SECTION_RE = re.compile(
    r"^\s{2,}([A-Z]\w*)\s*(?:\*\s*)?(?:\[[^\]]*\]\s*)?=\s*(?:object|ref\s+object|enum|tuple|concept|distinct|ref|ptr)",
    re.MULTILINE,
)
_OBJECT_OF_RE = re.compile(r"of\s+([A-Z]\w*)", re.MULTILINE)

_TYPE_REF_RE = re.compile(r"(?<![a-z_])([A-Z]\w{1,100})\b")
_FUNC_CALL_RE = re.compile(r"(?<!\w)([a-z_]\w{1,100})\s*[\(\[]")
_METHOD_CALL_RE = re.compile(r"\.([a-z_]\w{1,100})\s*[\(\[]")

_NIM_KEYWORDS = frozenset(
    {
        "if",
        "elif",
        "else",
        "when",
        "case",
        "of",
        "for",
        "while",
        "block",
        "break",
        "continue",
        "return",
        "yield",
        "discard",
        "raise",
        "try",
        "except",
        "finally",
        "defer",
        "proc",
        "func",
        "method",
        "iterator",
        "converter",
        "template",
        "macro",
        "type",
        "var",
        "let",
        "const",
        "import",
        "include",
        "from",
        "export",
        "object",
        "ref",
        "ptr",
        "enum",
        "tuple",
        "concept",
        "distinct",
        "mixin",
        "and",
        "or",
        "not",
        "xor",
        "shl",
        "shr",
        "div",
        "mod",
        "in",
        "notin",
        "is",
        "isnot",
        "as",
        "true",
        "false",
        "nil",
        "echo",
        "assert",
        "new",
        "result",
        "len",
        "add",
        "high",
        "low",
        "sizeof",
        "typeof",
        "repr",
    }
)

_NIM_COMMON_TYPES = frozenset(
    {
        "int",
        "int8",
        "int16",
        "int32",
        "int64",
        "uint",
        "uint8",
        "uint16",
        "uint32",
        "uint64",
        "float",
        "float32",
        "float64",
        "bool",
        "char",
        "string",
        "seq",
        "array",
        "openArray",
        "set",
        "HashSet",
        "Table",
        "OrderedTable",
        "CountTable",
        "ref",
        "ptr",
        "pointer",
        "cstring",
        "cint",
        "cfloat",
        "Natural",
        "Positive",
        "Slice",
        "HSlice",
        "Range",
        "RootObj",
        "RootEffect",
        "Exception",
        "IOError",
        "OSError",
        "ValueError",
    }
)

_DIFF_IMPORT_RE = re.compile(r"^\+\s*import\s+([\w/]+)", re.MULTILINE)
_DIFF_FROM_IMPORT_RE = re.compile(r"^\+\s*from\s+([\w/]+)\s+import", re.MULTILINE)
_DIFF_INCLUDE_RE = re.compile(r"^\+\s*include\s+([\w/]+)", re.MULTILINE)


def _is_nim_file(path: Path) -> bool:
    return path.suffix.lower() in _NIM_EXTS


def _extract_refs(content: str) -> set[str]:
    refs: set[str] = set()
    for m in _NIM_IMPORT_RE.finditer(content):
        refs.add(m.group(1))
    for m in _NIM_FROM_IMPORT_RE.finditer(content):
        refs.add(m.group(1))
    for m in _NIM_INCLUDE_RE.finditer(content):
        refs.add(m.group(1))
    return refs


def _extract_definitions(content: str) -> tuple[set[str], set[str]]:
    funcs: set[str] = set()
    for m in _PROC_RE.finditer(content):
        name = m.group(1)
        if name.lower() not in _NIM_KEYWORDS and len(name) >= 2:
            funcs.add(name)

    types: set[str] = set()
    types.update(m.group(1) for m in _TYPE_DEF_RE.finditer(content))
    types.update(m.group(1) for m in _TYPE_IN_SECTION_RE.finditer(content))

    return funcs, types


def _extract_inheritance(content: str) -> set[str]:
    return {m.group(1) for m in _OBJECT_OF_RE.finditer(content)}


def _extract_references(content: str) -> tuple[set[str], set[str]]:
    type_refs = {
        m.group(1) for m in _TYPE_REF_RE.finditer(content) if m.group(1).lower() not in {t.lower() for t in _NIM_COMMON_TYPES}
    }
    func_calls: set[str] = set()
    for m in _FUNC_CALL_RE.finditer(content):
        name = m.group(1)
        if name.lower() not in _NIM_KEYWORDS:
            func_calls.add(name)
    for m in _METHOD_CALL_RE.finditer(content):
        name = m.group(1)
        if name.lower() not in _NIM_KEYWORDS:
            func_calls.add(name)

    return type_refs, func_calls


def _collect_nim_refs(nim_files: list[Path]) -> set[str]:
    refs: set[str] = set()
    for nf in nim_files:
        try:
            content = nf.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        refs.update(_extract_refs(content))
    return refs


class NimEdgeBuilder(EdgeBuilder):
    weight = 0.60
    import_weight = EDGE_WEIGHTS["nim_import"].forward
    type_weight = EDGE_WEIGHTS["nim_type"].forward
    fn_weight = EDGE_WEIGHTS["nim_fn"].forward
    reverse_weight_factor = EDGE_WEIGHTS["nim_import"].reverse_factor

    def needs_from_diff(self, diff_content: str) -> list[str]:
        refs: list[str] = []
        for m in _DIFF_IMPORT_RE.finditer(diff_content):
            refs.append(m.group(1))
        for m in _DIFF_FROM_IMPORT_RE.finditer(diff_content):
            refs.append(m.group(1))
        for m in _DIFF_INCLUDE_RE.finditer(diff_content):
            refs.append(m.group(1))
        return refs

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        nim_files = [f for f in changed_files if _is_nim_file(f)]
        if not nim_files:
            return []

        refs = _collect_nim_refs(nim_files)
        return discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        nim_frags = [f for f in fragments if _is_nim_file(f.path)]
        if not nim_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)
        type_defs, fn_defs = self._build_indices(nim_frags)

        for nf in nim_frags:
            self._add_fragment_edges(nf, idx, type_defs, fn_defs, edges)

        return edges

    def _build_indices(self, nim_frags: list[Fragment]) -> tuple[dict[str, list[FragmentId]], dict[str, list[FragmentId]]]:
        type_defs: dict[str, list[FragmentId]] = defaultdict(list)
        fn_defs: dict[str, list[FragmentId]] = defaultdict(list)

        for f in nim_frags:
            funcs, types = _extract_definitions(f.content)
            for t in types:
                type_defs[t.lower()].append(f.id)
            for fn in funcs:
                fn_defs[fn.lower()].append(f.id)

        return type_defs, fn_defs

    def _add_fragment_edges(
        self,
        nf: Fragment,
        idx: FragmentIndex,
        type_defs: dict[str, list[FragmentId]],
        fn_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        self._add_import_edges(nf, idx, edges)
        self._add_inheritance_edges(nf, type_defs, edges)
        self._add_reference_edges(nf, type_defs, fn_defs, edges)

    def _add_import_edges(self, nf: Fragment, idx: FragmentIndex, edges: EdgeDict) -> None:
        for ref in _extract_refs(nf.content):
            ref_name = ref.split("/")[-1].lower()
            self.link_by_stem(nf.id, ref_name, idx, edges, self.import_weight)
            self.link_by_path_match(nf.id, ref, idx, edges, self.import_weight)

    def _add_inheritance_edges(self, nf: Fragment, type_defs: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        for parent in _extract_inheritance(nf.content):
            for fid in type_defs.get(parent.lower(), []):
                if fid != nf.id:
                    self.add_edge(edges, nf.id, fid, self.type_weight)

    def _add_reference_edges(
        self,
        nf: Fragment,
        type_defs: dict[str, list[FragmentId]],
        fn_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        type_refs, func_calls = _extract_references(nf.content)
        self_funcs, self_types = _extract_definitions(nf.content)
        self_type_lower = {t.lower() for t in self_types}
        self_fn_lower = {fn.lower() for fn in self_funcs}

        for type_ref in type_refs:
            if type_ref.lower() not in self_type_lower:
                for fid in type_defs.get(type_ref.lower(), []):
                    if fid != nf.id:
                        self.add_edge(edges, nf.id, fid, self.type_weight)

        for func_call in func_calls:
            if func_call.lower() not in self_fn_lower:
                for fid in fn_defs.get(func_call.lower(), []):
                    if fid != nf.id:
                        self.add_edge(edges, nf.id, fid, self.fn_weight)
