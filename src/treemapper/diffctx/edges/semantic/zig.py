from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_ZIG_EXTS = {".zig"}

_ZIG_IMPORT_RE = re.compile(
    r"@import\s{0,5}\(\s{0,5}[\"']([^\"']{1,300})[\"']\s{0,5}\)",
)

_ZIG_STD_MODULES = frozenset(
    {
        "std",
        "builtin",
        "root",
    }
)

_FN_DEF_RE = re.compile(r"^\s*(?:pub\s+|export\s+)?fn\s+([a-zA-Z_]\w*)\s*\(", re.MULTILINE)
_STRUCT_RE = re.compile(
    r"^\s*(?:(?:pub|export)\s+)?const\s+([A-Z]\w*)\s*=\s*(?:(?:extern|packed)\s+)?struct\b",
    re.MULTILINE,
)
_UNION_RE = re.compile(
    r"^\s*(?:(?:pub|export)\s+)?const\s+([A-Z]\w*)\s*=\s*(?:(?:extern|packed)\s+)?union\b",
    re.MULTILINE,
)
_ENUM_RE = re.compile(
    r"^\s*(?:pub\s+|export\s+)?const\s+([A-Z]\w*)\s*=\s*enum\b",
    re.MULTILINE,
)
_ERROR_SET_RE = re.compile(
    r"^\s*(?:pub\s+|export\s+)?const\s+([A-Z]\w*)\s*=\s*error\b",
    re.MULTILINE,
)
_TYPE_REF_RE = re.compile(r"(?<![a-z_])([A-Z]\w{1,100})\b")
_FUNC_CALL_RE = re.compile(r"(?<!\w)([a-z_]\w{1,100})\s*\(")
_METHOD_CALL_RE = re.compile(r"\.([a-z_]\w{1,100})\s*\(")

_ZIG_KEYWORDS = frozenset(
    {
        "if",
        "else",
        "while",
        "for",
        "switch",
        "return",
        "break",
        "continue",
        "defer",
        "errdefer",
        "unreachable",
        "try",
        "catch",
        "fn",
        "pub",
        "export",
        "extern",
        "inline",
        "noinline",
        "const",
        "var",
        "comptime",
        "volatile",
        "allowzero",
        "struct",
        "union",
        "enum",
        "error",
        "opaque",
        "usingnamespace",
        "test",
        "threadlocal",
        "and",
        "or",
        "orelse",
        "undefined",
        "null",
        "true",
        "false",
        "asm",
        "noalias",
        "align",
        "packed",
        "callconv",
    }
)

_ZIG_COMMON_TYPES = frozenset(
    {
        "u8",
        "u16",
        "u32",
        "u64",
        "u128",
        "usize",
        "i8",
        "i16",
        "i32",
        "i64",
        "i128",
        "isize",
        "f16",
        "f32",
        "f64",
        "f128",
        "bool",
        "void",
        "anyopaque",
        "anytype",
        "anyerror",
        "noreturn",
        "type",
        "comptime_int",
        "comptime_float",
    }
)

_DIFF_IMPORT_RE = re.compile(r"""^\+.*@import\s*\(\s*['"]([^'"]{1,300})['"]""", re.MULTILINE)


def _is_zig_file(path: Path) -> bool:
    return path.suffix.lower() in _ZIG_EXTS


def _extract_imports(content: str) -> set[str]:
    refs: set[str] = set()
    for m in _ZIG_IMPORT_RE.finditer(content):
        target = m.group(1)
        if target not in _ZIG_STD_MODULES:
            refs.add(target)
    return refs


def _extract_definitions(content: str) -> tuple[set[str], set[str]]:
    funcs: set[str] = set()
    for m in _FN_DEF_RE.finditer(content):
        name = m.group(1)
        if name not in _ZIG_KEYWORDS and len(name) >= 2:
            funcs.add(name)

    types: set[str] = set()
    types.update(m.group(1) for m in _STRUCT_RE.finditer(content))
    types.update(m.group(1) for m in _UNION_RE.finditer(content))
    types.update(m.group(1) for m in _ENUM_RE.finditer(content))
    types.update(m.group(1) for m in _ERROR_SET_RE.finditer(content))

    return funcs, types


def _extract_references(content: str) -> tuple[set[str], set[str]]:
    type_refs = {m.group(1) for m in _TYPE_REF_RE.finditer(content) if m.group(1) not in _ZIG_COMMON_TYPES}

    func_calls: set[str] = set()
    for m in _FUNC_CALL_RE.finditer(content):
        name = m.group(1)
        if name not in _ZIG_KEYWORDS:
            func_calls.add(name)
    for m in _METHOD_CALL_RE.finditer(content):
        name = m.group(1)
        if name not in _ZIG_KEYWORDS:
            func_calls.add(name)

    return type_refs, func_calls


def _import_to_name(ref: str) -> str:
    return Path(ref).stem.lower()


def _collect_zig_refs(zig_files: list[Path]) -> set[str]:
    refs: set[str] = set()
    for zf in zig_files:
        try:
            content = zf.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        for imp in _extract_imports(content):
            refs.add(imp)
            refs.add(_import_to_name(imp))
    return refs


class ZigEdgeBuilder(EdgeBuilder):
    weight = 0.60
    import_weight = EDGE_WEIGHTS["zig_import"].forward
    type_weight = EDGE_WEIGHTS["zig_type"].forward
    fn_weight = EDGE_WEIGHTS["zig_fn"].forward
    reverse_weight_factor = EDGE_WEIGHTS["zig_import"].reverse_factor

    def needs_from_diff(self, diff_content: str) -> list[str]:
        refs: list[str] = []
        for m in _DIFF_IMPORT_RE.finditer(diff_content):
            target = m.group(1)
            if target not in _ZIG_STD_MODULES:
                refs.append(target)
                refs.append(_import_to_name(target))
        return refs

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        zig_files = [f for f in changed_files if _is_zig_file(f)]
        if not zig_files:
            return []

        refs = _collect_zig_refs(zig_files)
        return discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        zig_frags = [f for f in fragments if _is_zig_file(f.path)]
        if not zig_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)
        type_defs, fn_defs = self._build_indices(zig_frags)

        for zf in zig_frags:
            self._add_fragment_edges(zf, idx, type_defs, fn_defs, edges)

        return edges

    def _build_indices(self, zig_frags: list[Fragment]) -> tuple[dict[str, list[FragmentId]], dict[str, list[FragmentId]]]:
        type_defs: dict[str, list[FragmentId]] = defaultdict(list)
        fn_defs: dict[str, list[FragmentId]] = defaultdict(list)

        for f in zig_frags:
            funcs, types = _extract_definitions(f.content)
            for t in types:
                type_defs[t.lower()].append(f.id)
            for fn in funcs:
                fn_defs[fn.lower()].append(f.id)

        return type_defs, fn_defs

    def _link_symbol_edges(
        self,
        frag_id: FragmentId,
        symbols: set[str],
        self_symbols: set[str],
        defs: dict[str, list[FragmentId]],
        weight: float,
        edges: EdgeDict,
    ) -> None:
        for sym in symbols:
            if sym.lower() not in self_symbols:
                for fid in defs.get(sym.lower(), []):
                    if fid != frag_id:
                        self.add_edge(edges, frag_id, fid, weight)

    def _add_fragment_edges(
        self,
        zf: Fragment,
        idx: FragmentIndex,
        type_defs: dict[str, list[FragmentId]],
        fn_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        imports = _extract_imports(zf.content)
        for imp in imports:
            ref_name = _import_to_name(imp)
            self.link_by_stem(zf.id, ref_name, idx, edges, self.import_weight)
            self.link_by_path_match(zf.id, imp, idx, edges, self.import_weight)

        type_refs, func_calls = _extract_references(zf.content)
        self_funcs, self_types = _extract_definitions(zf.content)
        self_type_lower = {t.lower() for t in self_types}
        self_fn_lower = {fn.lower() for fn in self_funcs}

        self._link_symbol_edges(zf.id, type_refs, self_type_lower, type_defs, self.type_weight, edges)
        self._link_symbol_edges(zf.id, func_calls, self_fn_lower, fn_defs, self.fn_weight, edges)
