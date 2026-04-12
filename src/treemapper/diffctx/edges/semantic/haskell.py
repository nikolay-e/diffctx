from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_HASKELL_EXTS = {".hs", ".lhs"}

_HASKELL_IMPORT_RE = re.compile(
    r"^\s{0,20}import\s{1,10}(?:qualified\s{1,10})?([A-Z][\w.]{0,200})",
    re.MULTILINE,
)

_FUNC_DEF_RE = re.compile(r"^([a-z_]\w*)\s*::", re.MULTILINE)
_FUNC_BIND_RE = re.compile(r"^([a-z_]\w*)\s+(?:[a-z_]\w*|\()", re.MULTILINE)
_DATA_RE = re.compile(r"^\s*data\s+([A-Z]\w*)", re.MULTILINE)
_NEWTYPE_RE = re.compile(r"^\s*newtype\s+([A-Z]\w*)", re.MULTILINE)
_TYPE_RE = re.compile(r"^\s*type\s+([A-Z]\w*)", re.MULTILINE)
_CLASS_RE = re.compile(r"^\s*class\s+(?:\([^)]*\)\s*=>\s*)?([A-Z]\w*)", re.MULTILINE)
_INSTANCE_RE = re.compile(
    r"^\s*instance\s+(?:\([^)]*\)\s*=>\s*)?([A-Z]\w*)\s+([A-Z]\w*)",
    re.MULTILINE,
)
_DERIVING_RE = re.compile(r"deriving\s*\(([^)]+)\)", re.MULTILINE)
_DERIVING_SINGLE_RE = re.compile(r"deriving\s+([A-Z]\w*)", re.MULTILINE)

_TYPE_REF_RE = re.compile(r"(?<![a-z_])([A-Z]\w{1,100})\b")
_FUNC_CALL_RE = re.compile(r"(?<!\w)([a-z_]\w{1,100})\b")

_HASKELL_KEYWORDS = frozenset(
    {
        "if",
        "then",
        "else",
        "let",
        "in",
        "where",
        "case",
        "of",
        "do",
        "return",
        "module",
        "import",
        "qualified",
        "as",
        "hiding",
        "data",
        "newtype",
        "type",
        "class",
        "instance",
        "deriving",
        "forall",
        "foreign",
        "default",
        "infixl",
        "infixr",
        "infix",
        "otherwise",
        "not",
        "and",
        "or",
        "map",
        "filter",
        "foldl",
        "foldr",
        "head",
        "tail",
        "null",
        "length",
        "show",
        "read",
        "pure",
        "print",
        "putStrLn",
        "putStr",
        "main",
        "undefined",
        "error",
        "seq",
    }
)

_HASKELL_COMMON_TYPES = frozenset(
    {
        "Int",
        "Integer",
        "Float",
        "Double",
        "Char",
        "String",
        "Bool",
        "IO",
        "Maybe",
        "Either",
        "Just",
        "Nothing",
        "Left",
        "Right",
        "True",
        "False",
        "Eq",
        "Ord",
        "Show",
        "Read",
        "Enum",
        "Bounded",
        "Num",
        "Real",
        "Integral",
        "Fractional",
        "Floating",
        "Functor",
        "Applicative",
        "Monad",
        "Monoid",
        "Semigroup",
    }
)

_DIFF_IMPORT_RE = re.compile(
    r"^\+\s*import\s+(?:qualified\s+)?([A-Z][\w.]{0,200})",
    re.MULTILINE,
)


def _is_haskell_file(path: Path) -> bool:
    return path.suffix.lower() in _HASKELL_EXTS


def _extract_imports(content: str) -> set[str]:
    return {m.group(1) for m in _HASKELL_IMPORT_RE.finditer(content)}


def _extract_definitions(content: str) -> tuple[set[str], set[str]]:
    types: set[str] = set()
    types.update(m.group(1) for m in _DATA_RE.finditer(content))
    types.update(m.group(1) for m in _NEWTYPE_RE.finditer(content))
    types.update(m.group(1) for m in _TYPE_RE.finditer(content))
    types.update(m.group(1) for m in _CLASS_RE.finditer(content))

    funcs: set[str] = set()
    for m in _FUNC_DEF_RE.finditer(content):
        name = m.group(1)
        if name not in _HASKELL_KEYWORDS and len(name) >= 2:
            funcs.add(name)
    for m in _FUNC_BIND_RE.finditer(content):
        name = m.group(1)
        if name not in _HASKELL_KEYWORDS and len(name) >= 2:
            funcs.add(name)

    return funcs, types


def _extract_instances(content: str) -> set[tuple[str, str]]:
    instances: set[tuple[str, str]] = set()
    for m in _INSTANCE_RE.finditer(content):
        instances.add((m.group(1), m.group(2)))
    return instances


def _extract_derivings(content: str) -> set[str]:
    derived: set[str] = set()
    for m in _DERIVING_RE.finditer(content):
        for part in m.group(1).split(","):
            name = part.strip()
            if name and name[0].isupper():
                derived.add(name)
    for m in _DERIVING_SINGLE_RE.finditer(content):
        derived.add(m.group(1))
    return derived


def _extract_references(content: str) -> tuple[set[str], set[str]]:
    type_refs = {m.group(1) for m in _TYPE_REF_RE.finditer(content) if m.group(1) not in _HASKELL_COMMON_TYPES}
    func_refs = {
        m.group(1) for m in _FUNC_CALL_RE.finditer(content) if m.group(1) not in _HASKELL_KEYWORDS and len(m.group(1)) >= 3
    }
    return type_refs, func_refs


def _module_to_path(module: str) -> str:
    return module.replace(".", "/")


def _module_leaf(module: str) -> str:
    return module.rsplit(".", maxsplit=1)[-1].lower()


def _collect_haskell_refs(haskell_files: list[Path]) -> set[str]:
    refs: set[str] = set()
    for hf in haskell_files:
        try:
            content = hf.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        for module in _extract_imports(content):
            refs.add(_module_to_path(module))
            refs.add(_module_leaf(module))
    return refs


class HaskellEdgeBuilder(EdgeBuilder):
    weight = 0.65
    import_weight = EDGE_WEIGHTS["haskell_import"].forward
    type_weight = EDGE_WEIGHTS["haskell_type"].forward
    fn_weight = EDGE_WEIGHTS["haskell_fn"].forward
    instance_weight = EDGE_WEIGHTS["haskell_instance"].forward
    reverse_weight_factor = EDGE_WEIGHTS["haskell_import"].reverse_factor

    def needs_from_diff(self, diff_content: str) -> list[str]:
        refs: list[str] = []
        for m in _DIFF_IMPORT_RE.finditer(diff_content):
            module = m.group(1)
            refs.append(_module_to_path(module))
            refs.append(_module_leaf(module))
        return refs

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        haskell_files = [f for f in changed_files if _is_haskell_file(f)]
        if not haskell_files:
            return []

        refs = _collect_haskell_refs(haskell_files)
        return discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        haskell_frags = [f for f in fragments if _is_haskell_file(f.path)]
        if not haskell_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)
        type_defs, fn_defs = self._build_indices(haskell_frags)

        for hf in haskell_frags:
            self._add_fragment_edges(hf, idx, type_defs, fn_defs, edges)

        return edges

    def _build_indices(self, haskell_frags: list[Fragment]) -> tuple[dict[str, list[FragmentId]], dict[str, list[FragmentId]]]:
        type_defs: dict[str, list[FragmentId]] = defaultdict(list)
        fn_defs: dict[str, list[FragmentId]] = defaultdict(list)

        for f in haskell_frags:
            funcs, types = _extract_definitions(f.content)
            for t in types:
                type_defs[t.lower()].append(f.id)
            for fn in funcs:
                fn_defs[fn.lower()].append(f.id)

        return type_defs, fn_defs

    def _add_fragment_edges(
        self,
        hf: Fragment,
        idx: FragmentIndex,
        type_defs: dict[str, list[FragmentId]],
        fn_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        self._add_import_edges(hf, idx, edges)
        self._add_instance_edges(hf, type_defs, edges)
        self._add_deriving_edges(hf, type_defs, edges)
        self._add_reference_edges(hf, type_defs, fn_defs, edges)

    def _add_import_edges(self, hf: Fragment, idx: FragmentIndex, edges: EdgeDict) -> None:
        for module in _extract_imports(hf.content):
            self.link_by_stem(hf.id, _module_leaf(module), idx, edges, self.import_weight)
            self.link_by_path_match(hf.id, _module_to_path(module), idx, edges, self.import_weight)

    def _add_instance_edges(
        self,
        hf: Fragment,
        type_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for type_class, type_name in _extract_instances(hf.content):
            for key in (type_class.lower(), type_name.lower()):
                for fid in type_defs.get(key, []):
                    if fid != hf.id:
                        self.add_edge(edges, hf.id, fid, self.instance_weight)

    def _add_deriving_edges(
        self,
        hf: Fragment,
        type_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for derived in _extract_derivings(hf.content):
            for fid in type_defs.get(derived.lower(), []):
                if fid != hf.id:
                    self.add_edge(edges, hf.id, fid, self.instance_weight)

    def _add_type_ref_edges(
        self,
        hf: Fragment,
        type_refs: set[str],
        type_defs: dict[str, list[FragmentId]],
        self_type_lower: set[str],
        edges: EdgeDict,
    ) -> None:
        for type_ref in type_refs:
            type_lower = type_ref.lower()
            if type_lower in self_type_lower:
                continue
            for fid in type_defs.get(type_lower, []):
                if fid != hf.id:
                    self.add_edge(edges, hf.id, fid, self.type_weight)

    def _add_fn_ref_edges(
        self,
        hf: Fragment,
        func_refs: set[str],
        fn_defs: dict[str, list[FragmentId]],
        self_fn_lower: set[str],
        edges: EdgeDict,
    ) -> None:
        for func_ref in func_refs:
            func_lower = func_ref.lower()
            if func_lower in self_fn_lower:
                continue
            for fid in fn_defs.get(func_lower, []):
                if fid != hf.id:
                    self.add_edge(edges, hf.id, fid, self.fn_weight)

    def _add_reference_edges(
        self,
        hf: Fragment,
        type_defs: dict[str, list[FragmentId]],
        fn_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        type_refs, func_refs = _extract_references(hf.content)
        self_funcs, self_types = _extract_definitions(hf.content)
        self_type_lower = {t.lower() for t in self_types}
        self_fn_lower = {fn.lower() for fn in self_funcs}

        self._add_type_ref_edges(hf, type_refs, type_defs, self_type_lower, edges)
        self._add_fn_ref_edges(hf, func_refs, fn_defs, self_fn_lower, edges)
