from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_LUA_EXTS = {".lua"}

_REQUIRE_RE = re.compile(r"""require\s*(?:\(\s*)?['"]([^'"]{1,300})['"]""", re.MULTILINE)
_DOFILE_RE = re.compile(r"""dofile\s*\(\s*['"]([^'"]{1,300})['"]""", re.MULTILINE)

_FUNC_DEF_RE = re.compile(r"^\s*(?:local\s+)?function\s+([a-zA-Z_]\w*(?:[.:]\w+)*)\s*\(", re.MULTILINE)
_VAR_FUNC_RE = re.compile(r"^\s*(?:local\s+)?([a-zA-Z_]\w*)\s*=\s*function\s*\(", re.MULTILINE)

_METHOD_CALL_RE = re.compile(r"(\w+):(\w+)\s*\(")
_DOT_CALL_RE = re.compile(r"(\w+)\.(\w+)\s*\(")
_FUNC_CALL_RE = re.compile(r"(?<!\w)([a-z_]\w{1,100})\s*\(")

_LUA_KEYWORDS = frozenset(
    {
        "if",
        "then",
        "else",
        "elseif",
        "end",
        "for",
        "while",
        "do",
        "repeat",
        "until",
        "return",
        "break",
        "local",
        "function",
        "and",
        "or",
        "not",
        "in",
        "nil",
        "true",
        "false",
        "print",
        "error",
        "assert",
        "type",
        "tostring",
        "tonumber",
        "pairs",
        "ipairs",
        "next",
        "select",
        "unpack",
        "table",
        "string",
        "math",
        "io",
        "os",
        "coroutine",
        "debug",
        "pcall",
        "xpcall",
        "setmetatable",
        "getmetatable",
        "rawget",
        "rawset",
        "rawequal",
        "rawlen",
        "require",
        "dofile",
        "loadfile",
    }
)

_DIFF_REQUIRE_RE = re.compile(r"""^\+.*?require\s*(?:\(\s*)?['"]([^'"]{1,300})['"]""", re.MULTILINE)
_DIFF_DOFILE_RE = re.compile(r"""^\+.*dofile\s*\(\s*['"]([^'"]{1,300})['"]""", re.MULTILINE)


def _is_lua_file(path: Path) -> bool:
    return path.suffix.lower() in _LUA_EXTS


def _extract_refs(content: str) -> set[str]:
    refs: set[str] = set()
    for m in _REQUIRE_RE.finditer(content):
        refs.add(m.group(1))
    for m in _DOFILE_RE.finditer(content):
        refs.add(m.group(1))
    return refs


def _extract_definitions(content: str) -> set[str]:
    funcs: set[str] = set()
    for m in _FUNC_DEF_RE.finditer(content):
        full_name = m.group(1)
        parts = re.split(r"[.:]", full_name)
        for part in parts:
            if part not in _LUA_KEYWORDS and len(part) >= 2:
                funcs.add(part)
    for m in _VAR_FUNC_RE.finditer(content):
        name = m.group(1)
        if name not in _LUA_KEYWORDS and len(name) >= 2:
            funcs.add(name)
    return funcs


def _extract_references(content: str) -> tuple[set[str], set[tuple[str, str]], set[tuple[str, str]]]:
    func_calls = {m.group(1) for m in _FUNC_CALL_RE.finditer(content) if m.group(1) not in _LUA_KEYWORDS}

    method_calls: set[tuple[str, str]] = set()
    for m in _METHOD_CALL_RE.finditer(content):
        method_calls.add((m.group(1), m.group(2)))

    dot_calls: set[tuple[str, str]] = set()
    for m in _DOT_CALL_RE.finditer(content):
        if m.group(1) not in _LUA_KEYWORDS:
            dot_calls.add((m.group(1), m.group(2)))

    return func_calls, method_calls, dot_calls


def _module_to_filename(module: str) -> str:
    return module.replace(".", "/").split("/")[-1].lower()


class LuaEdgeBuilder(EdgeBuilder):
    weight = 0.60
    require_weight = EDGE_WEIGHTS["lua_require"].forward
    fn_weight = EDGE_WEIGHTS["lua_fn"].forward
    method_weight = EDGE_WEIGHTS["lua_method"].forward
    reverse_weight_factor = EDGE_WEIGHTS["lua_require"].reverse_factor

    def needs_from_diff(self, diff_content: str) -> list[str]:
        refs: list[str] = []
        for m in _DIFF_REQUIRE_RE.finditer(diff_content):
            refs.append(_module_to_filename(m.group(1)))
        for m in _DIFF_DOFILE_RE.finditer(diff_content):
            refs.append(_module_to_filename(m.group(1)))
        return refs

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        lua_changed = [f for f in changed_files if _is_lua_file(f)]
        if not lua_changed:
            return []

        refs: set[str] = set()
        for f in lua_changed:
            try:
                content = f.read_text(encoding="utf-8")
                for module in _extract_refs(content):
                    refs.add(_module_to_filename(module))
            except (OSError, UnicodeDecodeError):
                continue

        return discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        lua_frags = [f for f in fragments if _is_lua_file(f.path)]
        if not lua_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)
        fn_defs = self._build_indices(lua_frags)

        for lf in lua_frags:
            self._add_fragment_edges(lf, idx, fn_defs, edges)

        return edges

    def _build_indices(self, lua_frags: list[Fragment]) -> dict[str, list[FragmentId]]:
        fn_defs: dict[str, list[FragmentId]] = defaultdict(list)

        for f in lua_frags:
            funcs = _extract_definitions(f.content)
            for fn in funcs:
                fn_defs[fn.lower()].append(f.id)

        return fn_defs

    def _add_fragment_edges(
        self,
        lf: Fragment,
        idx: FragmentIndex,
        fn_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for module in _extract_refs(lf.content):
            self._link_module(lf.id, module, idx, edges)

        self._add_call_edges(lf, fn_defs, edges)

    def _add_call_edges(
        self,
        lf: Fragment,
        fn_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        func_calls, method_calls, dot_calls = _extract_references(lf.content)
        self_fn_lower = {fn.lower() for fn in _extract_definitions(lf.content)}

        self._add_func_call_edges(lf, fn_defs, func_calls, self_fn_lower, edges)
        self._add_method_call_edges(lf, fn_defs, method_calls, edges)
        self._add_dot_call_edges(lf, fn_defs, dot_calls, edges)

    def _add_func_call_edges(
        self,
        lf: Fragment,
        fn_defs: dict[str, list[FragmentId]],
        func_calls: set[str],
        self_fn_lower: set[str],
        edges: EdgeDict,
    ) -> None:
        for func_call in func_calls:
            if func_call.lower() not in self_fn_lower:
                for fid in fn_defs.get(func_call.lower(), []):
                    if fid != lf.id:
                        self.add_edge(edges, lf.id, fid, self.fn_weight)

    def _add_method_call_edges(
        self,
        lf: Fragment,
        fn_defs: dict[str, list[FragmentId]],
        method_calls: set[tuple[str, str]],
        edges: EdgeDict,
    ) -> None:
        for obj, method in method_calls:
            for key in (obj.lower(), method.lower()):
                for fid in fn_defs.get(key, []):
                    if fid != lf.id:
                        self.add_edge(edges, lf.id, fid, self.method_weight)

    def _add_dot_call_edges(
        self,
        lf: Fragment,
        fn_defs: dict[str, list[FragmentId]],
        dot_calls: set[tuple[str, str]],
        edges: EdgeDict,
    ) -> None:
        for obj, method in dot_calls:
            for fid in fn_defs.get(obj.lower(), []):
                if fid != lf.id:
                    self.add_edge(edges, lf.id, fid, self.method_weight)
            for fid in fn_defs.get(method.lower(), []):
                if fid != lf.id:
                    self.add_edge(edges, lf.id, fid, self.fn_weight)

    def _link_module(
        self,
        src_id: FragmentId,
        module: str,
        idx: FragmentIndex,
        edges: EdgeDict,
    ) -> None:
        filename = _module_to_filename(module)
        for name, frag_ids in idx.by_name.items():
            stem = name.replace(".lua", "")
            if stem == filename:
                for fid in frag_ids:
                    if fid != src_id:
                        self.add_edge(edges, src_id, fid, self.require_weight)
                        return

        path_hint = module.replace(".", "/")
        self.link_by_path_match(src_id, path_hint, idx, edges, self.require_weight)
