from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, discover_files_by_refs

_ERLANG_EXTS = frozenset({".erl", ".hrl"})

_MODULE_RE = re.compile(r"^-module\(([a-z_][\w]{0,200})\)\.", re.MULTILINE)
_INCLUDE_RE = re.compile(r'^-include\("([^"]{1,300})"\)\.', re.MULTILINE)
_INCLUDE_LIB_RE = re.compile(r'^-include_lib\("([^"]{1,300})"\)\.', re.MULTILINE)
_BEHAVIOUR_RE = re.compile(r"^-behaviou?r\(([a-z_][\w]{0,200})\)\.", re.MULTILINE)
_IMPORT_RE = re.compile(r"^-import\(([a-z_][\w]{0,200})\s*,", re.MULTILINE)
_REMOTE_CALL_RE = re.compile(r"\b([a-z_][\w]{0,100}):([a-z_][\w]{0,100})\s*\(")
_FUNCTION_DEF_RE = re.compile(r"^([a-z_][\w]{0,100})\s*\(", re.MULTILINE)

_ERLANG_STDLIB_MODULES = frozenset(
    {
        "io",
        "lists",
        "maps",
        "string",
        "binary",
        "file",
        "gen_server",
        "gen_statem",
        "gen_event",
        "supervisor",
        "application",
        "ets",
        "timer",
        "erlang",
        "proplists",
        "dict",
        "sets",
        "gb_trees",
        "gb_sets",
        "queue",
        "ordsets",
        "orddict",
        "math",
        "re",
        "unicode",
        "calendar",
        "filename",
        "filelib",
        "os",
        "code",
        "sys",
        "proc_lib",
        "error_logger",
        "logger",
        "inet",
        "gen_tcp",
        "gen_udp",
        "ssl",
        "crypto",
        "rand",
        "httpc",
        "httpd",
    }
)


def _is_erlang_file(path: Path) -> bool:
    return path.suffix.lower() in _ERLANG_EXTS


def _extract_module_name(content: str) -> str | None:
    m = _MODULE_RE.search(content)
    return m.group(1) if m else None


def _extract_includes(content: str) -> set[str]:
    refs: set[str] = set()
    for m in _INCLUDE_RE.finditer(content):
        refs.add(m.group(1))
    for m in _INCLUDE_LIB_RE.finditer(content):
        refs.add(m.group(1))
    return refs


def _extract_behaviours(content: str) -> set[str]:
    return {m.group(1) for m in _BEHAVIOUR_RE.finditer(content)}


def _extract_imports(content: str) -> set[str]:
    return {m.group(1) for m in _IMPORT_RE.finditer(content)}


def _extract_remote_calls(content: str) -> set[str]:
    return {m.group(1) for m in _REMOTE_CALL_RE.finditer(content) if m.group(1) not in _ERLANG_STDLIB_MODULES}


def _include_to_filename(ref: str) -> str:
    return ref.split("/")[-1].lower()


class ErlangEdgeBuilder(EdgeBuilder):
    weight = 0.65
    include_weight = EDGE_WEIGHTS["erlang_include"].forward
    behaviour_weight = EDGE_WEIGHTS["erlang_behaviour"].forward
    call_weight = EDGE_WEIGHTS["erlang_call"].forward
    reverse_weight_factor = EDGE_WEIGHTS["erlang_include"].reverse_factor

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        erl_changed = [f for f in changed_files if _is_erlang_file(f)]
        if not erl_changed:
            return []

        refs: set[str] = set()
        for f in erl_changed:
            try:
                content = f.read_text(encoding="utf-8")
                for inc in _extract_includes(content):
                    refs.add(_include_to_filename(inc))
                for mod in _extract_remote_calls(content):
                    refs.add(f"{mod}.erl")
                for mod in _extract_imports(content):
                    refs.add(f"{mod}.erl")
                for beh in _extract_behaviours(content):
                    refs.add(f"{beh}.erl")
            except (OSError, UnicodeDecodeError):
                continue

        return discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        erl_frags = [f for f in fragments if _is_erlang_file(f.path)]
        if not erl_frags:
            return {}

        edges: EdgeDict = {}
        mod_to_frags = self._build_module_index(erl_frags)
        name_to_frags = self._build_name_index(erl_frags)

        for ef in erl_frags:
            self._link_fragment(ef, mod_to_frags, name_to_frags, edges)

        return edges

    def _build_module_index(self, erl_frags: list[Fragment]) -> dict[str, list[FragmentId]]:
        mod_to_frags: dict[str, list[FragmentId]] = defaultdict(list)
        for f in erl_frags:
            mod_name = _extract_module_name(f.content)
            if mod_name:
                mod_to_frags[mod_name.lower()].append(f.id)
            stem = f.path.stem.lower()
            mod_to_frags[stem].append(f.id)
        return mod_to_frags

    def _build_name_index(self, erl_frags: list[Fragment]) -> dict[str, list[FragmentId]]:
        name_to_frags: dict[str, list[FragmentId]] = defaultdict(list)
        for f in erl_frags:
            name_to_frags[f.path.name.lower()].append(f.id)
        return name_to_frags

    def _link_fragment(
        self,
        ef: Fragment,
        mod_to_frags: dict[str, list[FragmentId]],
        name_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        self._link_includes(ef, name_to_frags, edges)
        self._link_behaviours(ef, mod_to_frags, edges)
        self._link_imports(ef, mod_to_frags, edges)
        self._link_remote_calls(ef, mod_to_frags, edges)

    def _link_includes(
        self,
        ef: Fragment,
        name_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for inc in _extract_includes(ef.content):
            filename = _include_to_filename(inc)
            self.add_edges_from_ids(
                ef.id,
                name_to_frags.get(filename, []),
                self.include_weight,
                edges,
            )

    def _link_behaviours(
        self,
        ef: Fragment,
        mod_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for beh in _extract_behaviours(ef.content):
            self.add_edges_from_ids(
                ef.id,
                mod_to_frags.get(beh.lower(), []),
                self.behaviour_weight,
                edges,
            )

    def _link_imports(
        self,
        ef: Fragment,
        mod_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for mod in _extract_imports(ef.content):
            self.add_edges_from_ids(
                ef.id,
                mod_to_frags.get(mod.lower(), []),
                self.include_weight,
                edges,
            )

    def _link_remote_calls(
        self,
        ef: Fragment,
        mod_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for mod in _extract_remote_calls(ef.content):
            self.add_edges_from_ids(
                ef.id,
                mod_to_frags.get(mod.lower(), []),
                self.call_weight,
                edges,
            )
