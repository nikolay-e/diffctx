from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_OCAML_EXTS = {".ml", ".mli"}

_OCAML_OPEN_RE = re.compile(r"^\s*open!?\s+([A-Z][\w.]*)", re.MULTILINE)

_LET_RE = re.compile(r"^\s*let\s+(?:rec\s+)?([a-z_]\w*)\b", re.MULTILINE)
_VAL_RE = re.compile(r"^\s*val\s+([a-z_]\w*)\s*:", re.MULTILINE)
_TYPE_DEF_RE = re.compile(r"^\s*type\s+(?:'[a-z]\s+)?([a-z_]\w*)", re.MULTILINE)
_MODULE_DEF_RE = re.compile(r"^\s*module\s+([A-Z]\w*)", re.MULTILINE)
_MODULE_TYPE_DEF_RE = re.compile(r"^\s*module\s+type\s+([A-Z]\w*)", re.MULTILINE)
_FUNCTOR_APP_RE = re.compile(r"([A-Z]\w*)\s*\(", re.MULTILINE)
_INCLUDE_RE = re.compile(r"^\s*include\s+([A-Z]\w*)", re.MULTILINE)

_MODULE_REF_RE = re.compile(r"(?<![a-z_])([A-Z]\w*)\.")
_FUNC_CALL_RE = re.compile(r"(?<!\w)([a-z_]\w{1,100})\b")
_TYPE_ANNOTATION_RE = re.compile(r":\s*([a-z_]\w*)\b")

_OCAML_KEYWORDS = frozenset(
    {
        "if",
        "then",
        "else",
        "let",
        "rec",
        "in",
        "match",
        "with",
        "fun",
        "function",
        "begin",
        "end",
        "struct",
        "sig",
        "module",
        "open",
        "include",
        "type",
        "val",
        "mutable",
        "and",
        "or",
        "not",
        "mod",
        "land",
        "lor",
        "lxor",
        "lsl",
        "lsr",
        "asr",
        "as",
        "of",
        "when",
        "while",
        "for",
        "do",
        "done",
        "to",
        "downto",
        "try",
        "raise",
        "assert",
        "lazy",
        "new",
        "object",
        "method",
        "class",
        "virtual",
        "private",
        "inherit",
        "initializer",
        "true",
        "false",
        "ref",
        "unit",
        "print_endline",
        "print_string",
        "print_int",
        "print_float",
        "ignore",
        "failwith",
        "invalid_arg",
        "fst",
        "snd",
        "string_of_int",
        "int_of_string",
    }
)

_OCAML_COMMON_TYPES = frozenset(
    {
        "int",
        "float",
        "bool",
        "char",
        "string",
        "unit",
        "list",
        "array",
        "option",
        "result",
        "ref",
        "bytes",
        "exn",
        "nativeint",
        "int32",
        "int64",
    }
)

_DIFF_OPEN_RE = re.compile(r"^\+\s*open!?\s+([A-Z][\w.]*)", re.MULTILINE)
_DIFF_INCLUDE_RE = re.compile(r"^\+\s*include\s+([A-Z]\w*)", re.MULTILINE)


def _is_ocaml_file(path: Path) -> bool:
    return path.suffix.lower() in _OCAML_EXTS


def _extract_opens(content: str) -> set[str]:
    return {m.group(1) for m in _OCAML_OPEN_RE.finditer(content)}


def _extract_definitions(content: str) -> tuple[set[str], set[str], set[str]]:
    funcs: set[str] = set()
    for m in _LET_RE.finditer(content):
        name = m.group(1)
        if name not in _OCAML_KEYWORDS and len(name) >= 2:
            funcs.add(name)
    for m in _VAL_RE.finditer(content):
        name = m.group(1)
        if name not in _OCAML_KEYWORDS and len(name) >= 2:
            funcs.add(name)

    types: set[str] = set()
    for m in _TYPE_DEF_RE.finditer(content):
        name = m.group(1)
        if name not in _OCAML_COMMON_TYPES and len(name) >= 2:
            types.add(name)

    modules: set[str] = set()
    modules.update(m.group(1) for m in _MODULE_DEF_RE.finditer(content))
    modules.update(m.group(1) for m in _MODULE_TYPE_DEF_RE.finditer(content))

    return funcs, types, modules


def _extract_references(content: str) -> tuple[set[str], set[str], set[str]]:
    module_refs = {m.group(1) for m in _MODULE_REF_RE.finditer(content)}
    module_refs.update(m.group(1) for m in _FUNCTOR_APP_RE.finditer(content))
    module_refs.update(m.group(1) for m in _INCLUDE_RE.finditer(content))

    func_refs = {
        m.group(1) for m in _FUNC_CALL_RE.finditer(content) if m.group(1) not in _OCAML_KEYWORDS and len(m.group(1)) >= 3
    }

    type_refs = {
        m.group(1)
        for m in _TYPE_ANNOTATION_RE.finditer(content)
        if m.group(1) not in _OCAML_COMMON_TYPES and m.group(1) not in _OCAML_KEYWORDS
    }

    return module_refs, func_refs, type_refs


def _collect_ocaml_refs(ocaml_files: list[Path]) -> set[str]:
    refs: set[str] = set()
    for frag in ocaml_files:
        try:
            content = frag.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        refs.update(_extract_opens(content))
    return refs


class OCamlEdgeBuilder(EdgeBuilder):
    weight = 0.60
    open_weight = EDGE_WEIGHTS["ocaml_open"].forward
    type_weight = EDGE_WEIGHTS["ocaml_type"].forward
    fn_weight = EDGE_WEIGHTS["ocaml_fn"].forward
    module_ref_weight = EDGE_WEIGHTS["ocaml_module_ref"].forward
    reverse_weight_factor = EDGE_WEIGHTS["ocaml_open"].reverse_factor

    def needs_from_diff(self, diff_content: str) -> list[str]:
        refs: list[str] = []
        for m in _DIFF_OPEN_RE.finditer(diff_content):
            module = m.group(1)
            refs.append(module.split(".")[0].lower())
        for m in _DIFF_INCLUDE_RE.finditer(diff_content):
            refs.append(m.group(1).lower())
        return refs

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        ocaml_files = [f for f in changed_files if _is_ocaml_file(f)]
        if not ocaml_files:
            return []

        refs = _collect_ocaml_refs(ocaml_files)
        module_refs = set()
        for ref in refs:
            top_module = ref.split(".")[0].lower()
            module_refs.add(top_module)
        return discover_files_by_refs(module_refs, changed_files, all_candidate_files, repo_root)

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        ocaml_frags = [f for f in fragments if _is_ocaml_file(f.path)]
        if not ocaml_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)
        fn_defs, type_defs, module_defs = self._build_indices(ocaml_frags)

        for ocaml_frag in ocaml_frags:
            self._add_fragment_edges(ocaml_frag, idx, fn_defs, type_defs, module_defs, edges)

        return edges

    def _build_indices(
        self, ocaml_frags: list[Fragment]
    ) -> tuple[dict[str, list[FragmentId]], dict[str, list[FragmentId]], dict[str, list[FragmentId]]]:
        fn_defs: dict[str, list[FragmentId]] = defaultdict(list)
        type_defs: dict[str, list[FragmentId]] = defaultdict(list)
        module_defs: dict[str, list[FragmentId]] = defaultdict(list)

        for f in ocaml_frags:
            funcs, types, modules = _extract_definitions(f.content)
            for fn in funcs:
                fn_defs[fn.lower()].append(f.id)
            for t in types:
                type_defs[t.lower()].append(f.id)
            for mod in modules:
                module_defs[mod.lower()].append(f.id)

        return fn_defs, type_defs, module_defs

    def _link_opens(self, ocaml_frag: Fragment, idx: FragmentIndex, edges: EdgeDict) -> None:
        for opened in _extract_opens(ocaml_frag.content):
            module_name = opened.split(".")[0].lower()
            self._link_by_module(ocaml_frag.id, module_name, idx, edges)

    def _link_module_refs(self, ocaml_frag: Fragment, module_defs: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        module_refs, _, _ = _extract_references(ocaml_frag.content)
        for mod_ref in module_refs:
            for fid in module_defs.get(mod_ref.lower(), []):
                if fid != ocaml_frag.id:
                    self.add_edge(edges, ocaml_frag.id, fid, self.module_ref_weight)

    def _link_external_func_refs(
        self, ocaml_frag: Fragment, fn_defs: dict[str, list[FragmentId]], self_fn_lower: set[str], edges: EdgeDict
    ) -> None:
        _, func_refs, _ = _extract_references(ocaml_frag.content)
        for func_ref in func_refs:
            if func_ref.lower() in self_fn_lower:
                continue
            for fid in fn_defs.get(func_ref.lower(), []):
                if fid != ocaml_frag.id:
                    self.add_edge(edges, ocaml_frag.id, fid, self.fn_weight)

    def _link_external_type_refs(
        self, ocaml_frag: Fragment, type_defs: dict[str, list[FragmentId]], self_type_lower: set[str], edges: EdgeDict
    ) -> None:
        _, _, type_refs = _extract_references(ocaml_frag.content)
        for type_ref in type_refs:
            if type_ref.lower() in self_type_lower:
                continue
            for fid in type_defs.get(type_ref.lower(), []):
                if fid != ocaml_frag.id:
                    self.add_edge(edges, ocaml_frag.id, fid, self.type_weight)

    def _add_fragment_edges(
        self,
        ocaml_frag: Fragment,
        idx: FragmentIndex,
        fn_defs: dict[str, list[FragmentId]],
        type_defs: dict[str, list[FragmentId]],
        module_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        self._link_opens(ocaml_frag, idx, edges)
        self._link_module_refs(ocaml_frag, module_defs, edges)

        self_funcs, self_types, _ = _extract_definitions(ocaml_frag.content)
        self_fn_lower = {fn.lower() for fn in self_funcs}
        self_type_lower = {t.lower() for t in self_types}

        self._link_external_func_refs(ocaml_frag, fn_defs, self_fn_lower, edges)
        self._link_external_type_refs(ocaml_frag, type_defs, self_type_lower, edges)

    def _link_by_module(self, src_id: FragmentId, module_name: str, idx: FragmentIndex, edges: EdgeDict) -> None:
        for name, frag_ids in idx.by_name.items():
            stem = name.split(".")[0]
            if stem == module_name:
                for fid in frag_ids:
                    if fid != src_id:
                        self.add_edge(edges, src_id, fid, self.open_weight)
