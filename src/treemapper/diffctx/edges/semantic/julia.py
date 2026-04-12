from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_JULIA_EXTS = {".jl"}

_JULIA_USING_RE = re.compile(
    r"^\s{0,20}(?:using|import)\s{1,10}([\w.]{1,200})",
    re.MULTILINE,
)
_JULIA_INCLUDE_RE = re.compile(
    r"include\s{0,5}\(\s{0,5}[\"']([^\"']{1,300})[\"']\s{0,5}\)",
)

_JULIA_STDLIB = frozenset(
    {
        "base",
        "core",
        "main",
        "test",
        "pkg",
        "linearalgebra",
        "statistics",
        "random",
        "dates",
        "unicode",
        "markdown",
        "printf",
    }
)

_FUNC_DEF_RE = re.compile(r"^\s*(?:function|macro)\s+([a-zA-Z_]\w*)", re.MULTILINE)
_SHORT_FUNC_RE = re.compile(r"^([a-z_]\w*)\s*\([^)]*\)\s*=", re.MULTILINE)
_STRUCT_RE = re.compile(r"^\s*(?:mutable\s+)?struct\s+([A-Z]\w*)", re.MULTILINE)
_ABSTRACT_TYPE_RE = re.compile(r"^\s*abstract\s+type\s+([A-Z]\w*)", re.MULTILINE)
_PRIMITIVE_TYPE_RE = re.compile(r"^\s*primitive\s+type\s+([A-Z]\w*)", re.MULTILINE)

_SUBTYPE_RE = re.compile(r"<:\s*([A-Z]\w*)")
_TYPE_REF_RE = re.compile(r"(?<![a-z_])([A-Z]\w{1,100})\b")
_FUNC_CALL_RE = re.compile(r"(?<!\w)([a-z_]\w{1,100})\s*\(")
_JULIA_KEYWORDS = frozenset(
    {
        "if",
        "else",
        "elseif",
        "for",
        "while",
        "begin",
        "end",
        "function",
        "return",
        "break",
        "continue",
        "try",
        "catch",
        "finally",
        "throw",
        "let",
        "local",
        "global",
        "const",
        "module",
        "baremodule",
        "using",
        "import",
        "export",
        "struct",
        "mutable",
        "abstract",
        "primitive",
        "type",
        "true",
        "false",
        "nothing",
        "missing",
        "undef",
        "println",
        "print",
        "error",
        "typeof",
        "isa",
        "push!",
        "pop!",
        "append!",
        "length",
        "size",
        "map",
        "filter",
        "reduce",
        "collect",
        "zip",
        "range",
        "sum",
        "prod",
        "minimum",
        "maximum",
        "open",
        "close",
        "read",
        "write",
        "parse",
    }
)

_JULIA_COMMON_TYPES = frozenset(
    {
        "Int",
        "Int8",
        "Int16",
        "Int32",
        "Int64",
        "Int128",
        "UInt",
        "UInt8",
        "UInt16",
        "UInt32",
        "UInt64",
        "UInt128",
        "Float16",
        "Float32",
        "Float64",
        "Bool",
        "Char",
        "String",
        "Symbol",
        "Nothing",
        "Missing",
        "Any",
        "Number",
        "Real",
        "Integer",
        "AbstractFloat",
        "Complex",
        "Rational",
        "Array",
        "Vector",
        "Matrix",
        "Dict",
        "Set",
        "Tuple",
        "NamedTuple",
        "Pair",
        "IO",
        "IOBuffer",
        "Ref",
        "Type",
        "DataType",
        "Union",
        "UnionAll",
        "Function",
        "Method",
        "Module",
        "Expr",
        "Exception",
        "ErrorException",
    }
)

_DIFF_USING_RE = re.compile(r"^\+\s*(?:using|import)\s+([\w.]{1,200})", re.MULTILINE)
_DIFF_INCLUDE_RE = re.compile(r"^\+.*include\s*\(\s*[\"']([^\"']{1,300})[\"']", re.MULTILINE)


def _is_julia_file(path: Path) -> bool:
    return path.suffix.lower() in _JULIA_EXTS


def _extract_usings(content: str) -> set[str]:
    refs: set[str] = set()
    for m in _JULIA_USING_RE.finditer(content):
        module = m.group(1)
        if module.lower() not in _JULIA_STDLIB:
            refs.add(module)
    return refs


def _extract_includes(content: str) -> set[str]:
    return {m.group(1) for m in _JULIA_INCLUDE_RE.finditer(content)}


def _extract_definitions(content: str) -> tuple[set[str], set[str]]:
    funcs: set[str] = set()
    for m in _FUNC_DEF_RE.finditer(content):
        name = m.group(1)
        if name not in _JULIA_KEYWORDS and len(name) >= 2:
            funcs.add(name)
    for m in _SHORT_FUNC_RE.finditer(content):
        name = m.group(1)
        if name not in _JULIA_KEYWORDS and len(name) >= 2:
            funcs.add(name)

    types: set[str] = set()
    types.update(m.group(1) for m in _STRUCT_RE.finditer(content))
    types.update(m.group(1) for m in _ABSTRACT_TYPE_RE.finditer(content))
    types.update(m.group(1) for m in _PRIMITIVE_TYPE_RE.finditer(content))

    return funcs, types


def _extract_supertypes(content: str) -> set[str]:
    return {m.group(1) for m in _SUBTYPE_RE.finditer(content)}


def _extract_references(content: str) -> tuple[set[str], set[str]]:
    type_refs = {m.group(1) for m in _TYPE_REF_RE.finditer(content) if m.group(1) not in _JULIA_COMMON_TYPES}
    func_calls = {m.group(1) for m in _FUNC_CALL_RE.finditer(content) if m.group(1) not in _JULIA_KEYWORDS}
    return type_refs, func_calls


def _module_leaf(module: str) -> str:
    return module.rsplit(".", maxsplit=1)[-1].lower()


def _module_to_path(module: str) -> str:
    return module.replace(".", "/")


def _collect_julia_refs(julia_files: list[Path]) -> set[str]:
    refs: set[str] = set()
    for jf in julia_files:
        try:
            content = jf.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        for module in _extract_usings(content):
            refs.add(_module_to_path(module))
            refs.add(_module_leaf(module))
    return refs


class JuliaEdgeBuilder(EdgeBuilder):
    weight = 0.65
    using_weight = EDGE_WEIGHTS["julia_using"].forward
    include_weight = EDGE_WEIGHTS["julia_include"].forward
    type_weight = EDGE_WEIGHTS["julia_type"].forward
    fn_weight = EDGE_WEIGHTS["julia_fn"].forward
    reverse_weight_factor = EDGE_WEIGHTS["julia_using"].reverse_factor

    def needs_from_diff(self, diff_content: str) -> list[str]:
        refs: list[str] = []
        for m in _DIFF_USING_RE.finditer(diff_content):
            module = m.group(1)
            if module.lower() not in _JULIA_STDLIB:
                refs.append(_module_to_path(module))
                refs.append(_module_leaf(module))
        for m in _DIFF_INCLUDE_RE.finditer(diff_content):
            refs.append(m.group(1))
        return refs

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        julia_files = [f for f in changed_files if _is_julia_file(f)]
        if not julia_files:
            return []

        refs = _collect_julia_refs(julia_files)
        return discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        julia_frags = [f for f in fragments if _is_julia_file(f.path)]
        if not julia_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)
        type_defs, fn_defs = self._build_indices(julia_frags)

        for jf in julia_frags:
            self._add_fragment_edges(jf, idx, type_defs, fn_defs, edges)

        return edges

    def _build_indices(self, julia_frags: list[Fragment]) -> tuple[dict[str, list[FragmentId]], dict[str, list[FragmentId]]]:
        type_defs: dict[str, list[FragmentId]] = defaultdict(list)
        fn_defs: dict[str, list[FragmentId]] = defaultdict(list)

        for f in julia_frags:
            funcs, types = _extract_definitions(f.content)
            for t in types:
                type_defs[t.lower()].append(f.id)
            for fn in funcs:
                fn_defs[fn.lower()].append(f.id)

        return type_defs, fn_defs

    def _link_usings(self, jf: Fragment, idx: FragmentIndex, edges: EdgeDict) -> None:
        for module in _extract_usings(jf.content):
            leaf = _module_leaf(module)
            self.link_by_stem(jf.id, leaf, idx, edges, self.using_weight)
            self.link_by_path_match(jf.id, _module_to_path(module), idx, edges, self.using_weight)

    def _link_includes(self, jf: Fragment, idx: FragmentIndex, edges: EdgeDict) -> None:
        for inc in _extract_includes(jf.content):
            inc_name = Path(inc).stem.lower()
            self.link_by_stem(jf.id, inc_name, idx, edges, self.include_weight)
            self.link_by_path_match(jf.id, inc, idx, edges, self.include_weight)

    def _link_supertypes(self, jf: Fragment, type_defs: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        for supertype in _extract_supertypes(jf.content):
            for fid in type_defs.get(supertype.lower(), []):
                if fid != jf.id:
                    self.add_edge(edges, jf.id, fid, self.type_weight)

    def _link_external_type_refs(
        self, jf: Fragment, type_defs: dict[str, list[FragmentId]], self_type_lower: set[str], edges: EdgeDict
    ) -> None:
        type_refs, _ = _extract_references(jf.content)
        for type_ref in type_refs:
            if type_ref.lower() in self_type_lower:
                continue
            for fid in type_defs.get(type_ref.lower(), []):
                if fid != jf.id:
                    self.add_edge(edges, jf.id, fid, self.type_weight)

    def _link_external_func_calls(
        self, jf: Fragment, fn_defs: dict[str, list[FragmentId]], self_fn_lower: set[str], edges: EdgeDict
    ) -> None:
        _, func_calls = _extract_references(jf.content)
        for func_call in func_calls:
            if func_call.lower() in self_fn_lower:
                continue
            for fid in fn_defs.get(func_call.lower(), []):
                if fid != jf.id:
                    self.add_edge(edges, jf.id, fid, self.fn_weight)

    def _add_fragment_edges(
        self,
        jf: Fragment,
        idx: FragmentIndex,
        type_defs: dict[str, list[FragmentId]],
        fn_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        self._link_usings(jf, idx, edges)
        self._link_supertypes(jf, type_defs, edges)

        self_funcs, self_types = _extract_definitions(jf.content)
        self_type_lower = {t.lower() for t in self_types}
        self_fn_lower = {fn.lower() for fn in self_funcs}

        self._link_external_type_refs(jf, type_defs, self_type_lower, edges)
        self._link_external_func_calls(jf, fn_defs, self_fn_lower, edges)
