from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_DART_EXTS = {".dart"}

_IMPORT_RE = re.compile(r"^\s*import\s+['\"]([^'\"]{1,300})['\"]", re.MULTILINE)
_EXPORT_RE = re.compile(r"^\s*export\s+['\"]([^'\"]{1,300})['\"]", re.MULTILINE)
_PART_RE = re.compile(r"^\s*part\s+['\"]([^'\"]{1,300})['\"]", re.MULTILINE)
_PART_OF_RE = re.compile(r"^\s*part\s+of\s+['\"]([^'\"]{1,300})['\"]", re.MULTILINE)

_CLASS_RE = re.compile(r"^\s*(?:abstract\s+)?class\s+(\w+)", re.MULTILINE)
_MIXIN_RE = re.compile(r"^\s*mixin\s+(\w+)", re.MULTILINE)
_ENUM_RE = re.compile(r"^\s*enum\s+(\w+)", re.MULTILINE)
_TYPEDEF_RE = re.compile(r"^\s*typedef\s+(\w+)", re.MULTILINE)
_EXTENSION_RE = re.compile(r"^\s*extension\s+(\w+)\s+on\s+(\w+)", re.MULTILINE)
_TOP_FUNC_RE = re.compile(r"^(?:(?:[\w<>?,]+\s+)+)?([a-z_]\w*)\s*\(", re.MULTILINE)

_EXTENDS_RE = re.compile(r"\bextends\s+(\w+)")
_IMPLEMENTS_RE = re.compile(r"\bimplements\s+([^{]+)")
_WITH_RE = re.compile(r"\bwith\s+([^{]+)")

_TYPE_REF_RE = re.compile(r"(?<![a-z_])([A-Z]\w{1,100})\b")
_FUNC_CALL_RE = re.compile(r"(?<!\w)([a-z_]\w{1,100})\s*\(")

_DART_KEYWORDS = frozenset(
    {
        "if",
        "else",
        "for",
        "while",
        "do",
        "switch",
        "case",
        "return",
        "break",
        "continue",
        "throw",
        "try",
        "catch",
        "finally",
        "assert",
        "new",
        "const",
        "var",
        "final",
        "void",
        "dynamic",
        "class",
        "abstract",
        "extends",
        "implements",
        "with",
        "mixin",
        "enum",
        "typedef",
        "import",
        "export",
        "part",
        "library",
        "show",
        "hide",
        "as",
        "is",
        "in",
        "this",
        "super",
        "null",
        "true",
        "false",
        "async",
        "await",
        "yield",
        "print",
        "main",
        "get",
        "set",
    }
)

_DART_COMMON_TYPES = frozenset(
    {
        "String",
        "int",
        "double",
        "bool",
        "num",
        "List",
        "Map",
        "Set",
        "Future",
        "Stream",
        "Iterable",
        "Object",
        "Function",
        "Type",
        "Duration",
        "DateTime",
        "Null",
        "Never",
        "void",
        "dynamic",
    }
)

_DIFF_IMPORT_RE = re.compile(r"^\+\s*import\s+['\"]([^'\"]{1,300})['\"]", re.MULTILINE)
_DIFF_EXPORT_RE = re.compile(r"^\+\s*export\s+['\"]([^'\"]{1,300})['\"]", re.MULTILINE)
_DIFF_PART_RE = re.compile(r"^\+\s*part\s+['\"]([^'\"]{1,300})['\"]", re.MULTILINE)


def _is_dart_file(path: Path) -> bool:
    return path.suffix.lower() in _DART_EXTS


def _extract_refs(content: str) -> tuple[set[str], set[str], set[str]]:
    imports: set[str] = set()
    exports: set[str] = set()
    parts: set[str] = set()

    for m in _IMPORT_RE.finditer(content):
        imports.add(m.group(1))
    for m in _EXPORT_RE.finditer(content):
        exports.add(m.group(1))
    for m in _PART_RE.finditer(content):
        parts.add(m.group(1))
    for m in _PART_OF_RE.finditer(content):
        parts.add(m.group(1))

    return imports, exports, parts


def _extract_definitions(content: str) -> tuple[set[str], set[str]]:
    types: set[str] = set()
    types.update(m.group(1) for m in _CLASS_RE.finditer(content))
    types.update(m.group(1) for m in _MIXIN_RE.finditer(content))
    types.update(m.group(1) for m in _ENUM_RE.finditer(content))
    types.update(m.group(1) for m in _TYPEDEF_RE.finditer(content))
    for m in _EXTENSION_RE.finditer(content):
        types.add(m.group(1))

    funcs: set[str] = set()
    for m in _TOP_FUNC_RE.finditer(content):
        name = m.group(1)
        if name not in _DART_KEYWORDS and len(name) >= 2:
            funcs.add(name)

    return funcs, types


def _extract_inheritance(content: str) -> set[str]:
    parents: set[str] = set()
    for m in _EXTENDS_RE.finditer(content):
        parents.add(m.group(1))
    for m in _IMPLEMENTS_RE.finditer(content):
        for part in m.group(1).split(","):
            name = part.strip().split("<")[0].strip()
            if name and name[0].isupper():
                parents.add(name)
    for m in _WITH_RE.finditer(content):
        for part in m.group(1).split(","):
            name = part.strip().split("<")[0].strip()
            if name and name[0].isupper():
                parents.add(name)
    return parents


def _extract_references(content: str) -> tuple[set[str], set[str]]:
    type_refs = {m.group(1) for m in _TYPE_REF_RE.finditer(content) if m.group(1) not in _DART_COMMON_TYPES}
    func_calls = {m.group(1) for m in _FUNC_CALL_RE.finditer(content) if m.group(1) not in _DART_KEYWORDS}
    return type_refs, func_calls


def _ref_to_filename(ref: str) -> str:
    name = ref.split("/")[-1].lower()
    if name.startswith("package:"):
        name = name.split(":")[-1]
    return name


class DartEdgeBuilder(EdgeBuilder):
    weight = 0.65
    import_weight = EDGE_WEIGHTS["dart_import"].forward
    export_weight = EDGE_WEIGHTS["dart_export"].forward
    type_weight = EDGE_WEIGHTS["dart_type"].forward
    fn_weight = EDGE_WEIGHTS["dart_fn"].forward
    inheritance_weight = EDGE_WEIGHTS["dart_inheritance"].forward
    reverse_weight_factor = EDGE_WEIGHTS["dart_import"].reverse_factor

    def needs_from_diff(self, diff_content: str) -> list[str]:
        refs: list[str] = []
        for m in _DIFF_IMPORT_RE.finditer(diff_content):
            refs.append(_ref_to_filename(m.group(1)))
        for m in _DIFF_EXPORT_RE.finditer(diff_content):
            refs.append(_ref_to_filename(m.group(1)))
        for m in _DIFF_PART_RE.finditer(diff_content):
            refs.append(_ref_to_filename(m.group(1)))
        return refs

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        dart_changed = [f for f in changed_files if _is_dart_file(f)]
        if not dart_changed:
            return []

        refs: set[str] = set()
        for f in dart_changed:
            try:
                content = f.read_text(encoding="utf-8")
                imports, exports, parts = _extract_refs(content)
                for r in imports | exports | parts:
                    refs.add(_ref_to_filename(r))
            except (OSError, UnicodeDecodeError):
                continue

        return discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        dart_frags = [f for f in fragments if _is_dart_file(f.path)]
        if not dart_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)
        type_defs, fn_defs = self._build_indices(dart_frags)

        for df in dart_frags:
            self._add_fragment_edges(df, idx, type_defs, fn_defs, edges)

        return edges

    def _build_indices(self, dart_frags: list[Fragment]) -> tuple[dict[str, list[FragmentId]], dict[str, list[FragmentId]]]:
        type_defs: dict[str, list[FragmentId]] = defaultdict(list)
        fn_defs: dict[str, list[FragmentId]] = defaultdict(list)

        for f in dart_frags:
            funcs, types = _extract_definitions(f.content)
            for t in types:
                type_defs[t.lower()].append(f.id)
            for fn in funcs:
                fn_defs[fn.lower()].append(f.id)

        return type_defs, fn_defs

    def _link_imports_exports_parts(
        self,
        df: Fragment,
        idx: FragmentIndex,
        edges: EdgeDict,
    ) -> None:
        imports, exports, parts = _extract_refs(df.content)
        for ref in imports:
            self._link_ref(df.id, ref, idx, edges, self.import_weight)
        for ref in exports:
            self._link_ref(df.id, ref, idx, edges, self.export_weight)
        for ref in parts:
            self._link_ref(df.id, ref, idx, edges, self.import_weight)

    def _link_inheritance(
        self,
        df: Fragment,
        type_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for parent in _extract_inheritance(df.content):
            for fid in type_defs.get(parent.lower(), []):
                if fid != df.id:
                    self.add_edge(edges, df.id, fid, self.inheritance_weight)

    def _link_type_refs(
        self,
        df: Fragment,
        type_defs: dict[str, list[FragmentId]],
        self_type_lower: set[str],
        edges: EdgeDict,
    ) -> None:
        type_refs, _ = _extract_references(df.content)
        for type_ref in type_refs:
            if type_ref.lower() in self_type_lower:
                continue
            for fid in type_defs.get(type_ref.lower(), []):
                if fid != df.id:
                    self.add_edge(edges, df.id, fid, self.type_weight)

    def _link_fn_calls(
        self,
        df: Fragment,
        fn_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        _, func_calls = _extract_references(df.content)
        for func_call in func_calls:
            for fid in fn_defs.get(func_call.lower(), []):
                if fid != df.id:
                    self.add_edge(edges, df.id, fid, self.fn_weight)

    def _add_fragment_edges(
        self,
        df: Fragment,
        idx: FragmentIndex,
        type_defs: dict[str, list[FragmentId]],
        fn_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        self._link_imports_exports_parts(df, idx, edges)
        self._link_inheritance(df, type_defs, edges)

        _, self_types = _extract_definitions(df.content)
        self_type_lower = {t.lower() for t in self_types}

        self._link_type_refs(df, type_defs, self_type_lower, edges)
        self._link_fn_calls(df, fn_defs, edges)

    def _link_ref(
        self,
        src_id: FragmentId,
        ref: str,
        idx: FragmentIndex,
        edges: EdgeDict,
        weight: float,
    ) -> None:
        filename = _ref_to_filename(ref)
        for name, frag_ids in idx.by_name.items():
            if name == filename or name == filename.replace(".dart", ""):
                for fid in frag_ids:
                    if fid != src_id:
                        self.add_edge(edges, src_id, fid, weight)
                        return

        self.link_by_path_match(src_id, ref, idx, edges, weight)
