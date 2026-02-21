from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict

_RUST_USE_RE = re.compile(r"^\s*use\s+(?:crate::)?([a-zA-Z_]\w*(?:::[a-zA-Z_]\w*)*)", re.MULTILINE)
_RUST_MOD_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([a-z_][a-z0-9_]*)\s*[;{]", re.MULTILINE)
_RUST_EXTERN_CRATE_RE = re.compile(r"^\s*extern\s+crate\s+([a-z_][a-z0-9_]*)", re.MULTILINE)

_RUST_FN_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([a-z_][a-z0-9_]*)", re.MULTILINE)
_RUST_STRUCT_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?struct\s+([A-Z]\w*)", re.MULTILINE)
_RUST_ENUM_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?enum\s+([A-Z]\w*)", re.MULTILINE)
_RUST_TRAIT_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?trait\s+([A-Z]\w*)", re.MULTILINE)
_RUST_IMPL_RE = re.compile(r"^\s*impl(?:<[^>]+>)?\s+(?:\w+\s+for\s+)?([A-Z]\w*)", re.MULTILINE)
_RUST_TYPE_ALIAS_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?type\s+([A-Z]\w*)", re.MULTILINE)

_RUST_TYPE_REF_RE = re.compile(r"(?<![a-z_])([A-Z]\w*)\b")
_RUST_FN_CALL_RE = re.compile(r"(?<!\w)([a-z_][a-z0-9_]*)\s?!?\s?\(")
_RUST_PATH_CALL_RE = re.compile(r"([a-z_][a-z0-9_]*)::([a-z_][a-z0-9_]*|[A-Z]\w*)")


def _is_rust_file(path: Path) -> bool:
    return path.suffix.lower() == ".rs"


def _extract_uses(content: str) -> set[str]:
    uses: set[str] = set()
    for match in _RUST_USE_RE.finditer(content):
        path = match.group(1)
        uses.add(path)
        parts = path.split("::")
        if len(parts) > 1:
            uses.add(parts[0])
    return uses


def _extract_mods(content: str) -> set[str]:
    return {m.group(1) for m in _RUST_MOD_RE.finditer(content)}


def _extract_definitions(content: str) -> tuple[set[str], set[str], set[str]]:
    funcs = {m.group(1) for m in _RUST_FN_RE.finditer(content)}
    types: set[str] = set()
    types.update(m.group(1) for m in _RUST_STRUCT_RE.finditer(content))
    types.update(m.group(1) for m in _RUST_ENUM_RE.finditer(content))
    types.update(m.group(1) for m in _RUST_TRAIT_RE.finditer(content))
    types.update(m.group(1) for m in _RUST_TYPE_ALIAS_RE.finditer(content))

    for m in _RUST_IMPL_RE.finditer(content):
        if m.group(1):
            types.add(m.group(1))

    traits = {m.group(1) for m in _RUST_TRAIT_RE.finditer(content)}
    return funcs, types, traits


def _extract_references(content: str) -> tuple[set[str], set[str], set[tuple[str, str]]]:
    type_refs = {m.group(1) for m in _RUST_TYPE_REF_RE.finditer(content)}
    fn_calls = {m.group(1) for m in _RUST_FN_CALL_RE.finditer(content)}
    path_calls = {(m.group(1), m.group(2)) for m in _RUST_PATH_CALL_RE.finditer(content)}
    return type_refs, fn_calls, path_calls


class RustEdgeBuilder(EdgeBuilder):
    weight = 0.75
    mod_weight = EDGE_WEIGHTS["rust_mod"].forward
    use_weight = EDGE_WEIGHTS["rust_use"].forward
    type_weight = EDGE_WEIGHTS["rust_type"].forward
    fn_weight = EDGE_WEIGHTS["rust_fn"].forward
    same_crate_weight = EDGE_WEIGHTS["rust_same_crate"].forward
    reverse_weight_factor = EDGE_WEIGHTS["rust_mod"].reverse_factor

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        rust_frags = [f for f in fragments if _is_rust_file(f.path)]
        if not rust_frags:
            return {}

        edges: EdgeDict = {}
        indices = self._build_indices(rust_frags)

        for rf in rust_frags:
            self._link_fragment(rf, rust_frags, indices, edges)

        return edges

    def _build_indices(
        self, rust_frags: list[Fragment]
    ) -> tuple[
        dict[str, list[FragmentId]], dict[str, list[FragmentId]], dict[str, list[FragmentId]], dict[str, list[FragmentId]]
    ]:
        name_to_frags: dict[str, list[FragmentId]] = defaultdict(list)
        mod_to_frags: dict[str, list[FragmentId]] = defaultdict(list)
        type_defs: dict[str, list[FragmentId]] = defaultdict(list)
        fn_defs: dict[str, list[FragmentId]] = defaultdict(list)

        for f in rust_frags:
            stem = f.path.stem.lower()
            name_to_frags[stem].append(f.id)

            if stem in {"mod", "lib"}:
                mod_to_frags[f.path.parent.name.lower()].append(f.id)
            else:
                mod_to_frags[stem].append(f.id)

            funcs, types, _ = _extract_definitions(f.content)
            for t in types:
                type_defs[t.lower()].append(f.id)
            for fn in funcs:
                fn_defs[fn.lower()].append(f.id)

            for mod_name in _extract_mods(f.content):
                mod_to_frags[mod_name.lower()].append(f.id)

        return name_to_frags, mod_to_frags, type_defs, fn_defs

    def _link_fragment(
        self,
        rf: Fragment,
        rust_frags: list[Fragment],
        indices: tuple[
            dict[str, list[FragmentId]], dict[str, list[FragmentId]], dict[str, list[FragmentId]], dict[str, list[FragmentId]]
        ],
        edges: EdgeDict,
    ) -> None:
        name_to_frags, mod_to_frags, type_defs, fn_defs = indices

        self._link_uses(rf, mod_to_frags, name_to_frags, edges)
        self._link_declared_mods(rf, name_to_frags, edges)
        self._link_refs(rf, type_defs, fn_defs, edges)
        self._link_path_calls(rf, mod_to_frags, edges)
        self._link_same_crate(rf, rust_frags, edges)

    def _link_uses(
        self,
        rf: Fragment,
        mod_to_frags: dict[str, list[FragmentId]],
        name_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for use_path in _extract_uses(rf.content):
            self._link_use_path_parts(rf.id, use_path, mod_to_frags, name_to_frags, edges)

    def _link_use_path_parts(
        self,
        rf_id: FragmentId,
        use_path: str,
        mod_to_frags: dict[str, list[FragmentId]],
        name_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for part in use_path.split("::"):
            part_lower = part.lower()
            self.add_edges_from_ids(rf_id, mod_to_frags.get(part_lower, []), self.use_weight, edges)
            self.add_edges_from_ids(rf_id, name_to_frags.get(part_lower, []), self.use_weight, edges)

    def _link_declared_mods(
        self,
        rf: Fragment,
        name_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for mod_name in _extract_mods(rf.content):
            for fid in name_to_frags.get(mod_name.lower(), []):
                if fid != rf.id:
                    self.add_edge(edges, rf.id, fid, self.mod_weight)

    def _link_refs(
        self,
        rf: Fragment,
        type_defs: dict[str, list[FragmentId]],
        fn_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        type_refs, fn_calls, _ = _extract_references(rf.content)

        for type_ref in type_refs:
            for fid in type_defs.get(type_ref.lower(), []):
                if fid != rf.id:
                    self.add_edge(edges, rf.id, fid, self.type_weight)

        for fn_call in fn_calls:
            for fid in fn_defs.get(fn_call.lower(), []):
                if fid != rf.id:
                    self.add_edge(edges, rf.id, fid, self.fn_weight)

    def _link_path_calls(
        self,
        rf: Fragment,
        mod_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        _, _, path_calls = _extract_references(rf.content)
        for mod_name, _symbol in path_calls:
            for fid in mod_to_frags.get(mod_name.lower(), []):
                if fid != rf.id:
                    self.add_edge(edges, rf.id, fid, self.use_weight)

    def _link_same_crate(
        self,
        rf: Fragment,
        rust_frags: list[Fragment],
        edges: EdgeDict,
    ) -> None:
        if rf.path.stem.lower() not in {"lib", "main", "mod"}:
            return
        parent_dir = rf.path.parent
        for f in rust_frags:
            if f.path.parent == parent_dir and f.id != rf.id:
                self.add_edge(edges, rf.id, f.id, self.same_crate_weight)
