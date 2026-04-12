from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_ELIXIR_EXTS = {".ex", ".exs"}

_USE_RE = re.compile(r"^\s*(?:use|alias|import|require)\s+([\w.]{2,200})", re.MULTILINE)

_BEHAVIOUR_RE = re.compile(r"^\s*@behaviour\s+([\w.]{2,200})", re.MULTILINE)
_DEFIMPL_RE = re.compile(r"defimpl\s+([\w.]+),\s*for:\s*([\w.]+)", re.MULTILINE)
_DEFMODULE_RE = re.compile(r"^\s*defmodule\s+([\w.]+)", re.MULTILINE)
_DEFPROTOCOL_RE = re.compile(r"^\s*defprotocol\s+([\w.]+)", re.MULTILINE)
_DEF_RE = re.compile(r"^\s*(?:def|defp|defmacro|defmacrop)\s+([a-z_]\w*)", re.MULTILINE)
_FUNC_CALL_RE = re.compile(r"(?<!\w)([a-z_]\w{1,100})\s*\(")
_MODULE_REF_RE = re.compile(r"(?<![a-z_])([A-Z]\w*(?:\.\w+)*)\b")

_ELIXIR_KEYWORDS = frozenset(
    {
        "if",
        "unless",
        "cond",
        "case",
        "when",
        "and",
        "or",
        "not",
        "in",
        "fn",
        "do",
        "end",
        "else",
        "after",
        "rescue",
        "catch",
        "raise",
        "throw",
        "receive",
        "with",
        "for",
        "quote",
        "unquote",
        "def",
        "defp",
        "defmodule",
        "defmacro",
        "defmacrop",
        "defprotocol",
        "defimpl",
        "defstruct",
        "defdelegate",
        "defguard",
        "defexception",
        "use",
        "alias",
        "import",
        "require",
        "true",
        "false",
        "nil",
    }
)

_ELIXIR_COMMON_MODULES = frozenset(
    {
        "Enum",
        "Map",
        "List",
        "String",
        "IO",
        "File",
        "Path",
        "Keyword",
        "Tuple",
        "Agent",
        "Task",
        "GenServer",
        "Supervisor",
        "Logger",
        "Kernel",
        "Process",
        "Module",
        "Macro",
    }
)

_DIFF_USE_RE = re.compile(r"^\+\s*(?:use|alias|import|require)\s+([\w.]{2,200})", re.MULTILINE)
_DIFF_BEHAVIOUR_RE = re.compile(r"^\+\s*@behaviour\s+([\w.]{2,200})", re.MULTILINE)


def _is_elixir_file(path: Path) -> bool:
    return path.suffix.lower() in _ELIXIR_EXTS


def _extract_refs(content: str) -> tuple[set[str], set[str]]:
    uses: set[str] = set()
    aliases: set[str] = set()

    for m in _USE_RE.finditer(content):
        module = m.group(1)
        line = m.group(0).strip()
        if line.startswith("alias"):
            aliases.add(module)
        else:
            uses.add(module)

    return uses, aliases


def _extract_definitions(content: str) -> tuple[set[str], set[str], set[str]]:
    modules: set[str] = set()
    modules.update(m.group(1) for m in _DEFMODULE_RE.finditer(content))
    modules.update(m.group(1) for m in _DEFPROTOCOL_RE.finditer(content))

    funcs: set[str] = set()
    for m in _DEF_RE.finditer(content):
        name = m.group(1)
        if name not in _ELIXIR_KEYWORDS and len(name) >= 2:
            funcs.add(name)

    behaviours: set[str] = set()
    for m in _BEHAVIOUR_RE.finditer(content):
        behaviours.add(m.group(1))
    for m in _DEFIMPL_RE.finditer(content):
        behaviours.add(m.group(1))
        behaviours.add(m.group(2))

    return funcs, modules, behaviours


def _extract_references(content: str) -> tuple[set[str], set[str]]:
    module_refs = {
        m.group(1).split(".")[0]
        for m in _MODULE_REF_RE.finditer(content)
        if m.group(1).split(".")[0] not in _ELIXIR_COMMON_MODULES
    }

    func_calls = {m.group(1) for m in _FUNC_CALL_RE.finditer(content) if m.group(1) not in _ELIXIR_KEYWORDS}

    return module_refs, func_calls


def _module_to_filename(module: str) -> str:
    parts = module.split(".")
    last = parts[-1]
    result: list[str] = []
    for i, ch in enumerate(last):
        if ch.isupper() and i > 0 and last[i - 1].islower():
            result.append("_")
        result.append(ch.lower())
    return "".join(result)


class ElixirEdgeBuilder(EdgeBuilder):
    weight = 0.65
    use_weight = EDGE_WEIGHTS["elixir_use"].forward
    alias_weight = EDGE_WEIGHTS["elixir_alias"].forward
    behaviour_weight = EDGE_WEIGHTS["elixir_behaviour"].forward
    fn_weight = EDGE_WEIGHTS["elixir_fn"].forward
    reverse_weight_factor = EDGE_WEIGHTS["elixir_use"].reverse_factor

    def needs_from_diff(self, diff_content: str) -> list[str]:
        refs: list[str] = []
        for m in _DIFF_USE_RE.finditer(diff_content):
            refs.append(_module_to_filename(m.group(1)))
        for m in _DIFF_BEHAVIOUR_RE.finditer(diff_content):
            refs.append(_module_to_filename(m.group(1)))
        return refs

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        elixir_changed = [f for f in changed_files if _is_elixir_file(f)]
        if not elixir_changed:
            return []

        refs: set[str] = set()
        for f in elixir_changed:
            try:
                content = f.read_text(encoding="utf-8")
                uses, aliases = _extract_refs(content)
                for module in uses | aliases:
                    refs.add(_module_to_filename(module))
            except (OSError, UnicodeDecodeError):
                continue

        return discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        elixir_frags = [f for f in fragments if _is_elixir_file(f.path)]
        if not elixir_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)
        module_defs, fn_defs = self._build_indices(elixir_frags)

        for ef in elixir_frags:
            self._add_fragment_edges(ef, idx, module_defs, fn_defs, edges)

        return edges

    def _build_indices(self, elixir_frags: list[Fragment]) -> tuple[dict[str, list[FragmentId]], dict[str, list[FragmentId]]]:
        module_defs: dict[str, list[FragmentId]] = defaultdict(list)
        fn_defs: dict[str, list[FragmentId]] = defaultdict(list)

        for f in elixir_frags:
            funcs, modules, _ = _extract_definitions(f.content)
            for mod in modules:
                module_defs[mod.split(".")[-1].lower()].append(f.id)
                module_defs[_module_to_filename(mod)].append(f.id)
            for fn in funcs:
                fn_defs[fn.lower()].append(f.id)

        return module_defs, fn_defs

    def _link_uses_and_aliases(
        self,
        ef: Fragment,
        idx: FragmentIndex,
        edges: EdgeDict,
    ) -> None:
        uses, aliases = _extract_refs(ef.content)
        for module in uses:
            self._link_module(ef.id, module, idx, edges, self.use_weight)
        for module in aliases:
            self._link_module(ef.id, module, idx, edges, self.alias_weight)

    def _link_behaviours(
        self,
        ef: Fragment,
        module_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        _, _, behaviours = _extract_definitions(ef.content)
        for behaviour in behaviours:
            for fid in module_defs.get(_module_to_filename(behaviour), []):
                if fid != ef.id:
                    self.add_edge(edges, ef.id, fid, self.behaviour_weight)

    def _link_module_refs(
        self,
        ef: Fragment,
        module_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        module_refs, _ = _extract_references(ef.content)
        for ref in module_refs:
            for fid in module_defs.get(_module_to_filename(ref), []):
                if fid != ef.id:
                    self.add_edge(edges, ef.id, fid, self.use_weight)

    def _link_fn_calls(
        self,
        ef: Fragment,
        fn_defs: dict[str, list[FragmentId]],
        self_fn_lower: set[str],
        edges: EdgeDict,
    ) -> None:
        _, func_calls = _extract_references(ef.content)
        for func_call in func_calls:
            if func_call.lower() in self_fn_lower:
                continue
            for fid in fn_defs.get(func_call.lower(), []):
                if fid != ef.id:
                    self.add_edge(edges, ef.id, fid, self.fn_weight)

    def _add_fragment_edges(
        self,
        ef: Fragment,
        idx: FragmentIndex,
        module_defs: dict[str, list[FragmentId]],
        fn_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        self._link_uses_and_aliases(ef, idx, edges)
        self._link_behaviours(ef, module_defs, edges)
        self._link_module_refs(ef, module_defs, edges)

        self_funcs, _, _ = _extract_definitions(ef.content)
        self_fn_lower = {fn.lower() for fn in self_funcs}
        self._link_fn_calls(ef, fn_defs, self_fn_lower, edges)

    def _link_module(
        self,
        src_id: FragmentId,
        module: str,
        idx: FragmentIndex,
        edges: EdgeDict,
        weight: float,
    ) -> None:
        filename = _module_to_filename(module)
        for name, frag_ids in idx.by_name.items():
            stem = name.replace(".ex", "").replace(".exs", "")
            if stem == filename:
                for fid in frag_ids:
                    if fid != src_id:
                        self.add_edge(edges, src_id, fid, weight)
                        return

        path_hint = module.replace(".", "/").lower()
        self.link_by_path_match(src_id, path_hint, idx, edges, weight)
