from __future__ import annotations

from collections import defaultdict
from pathlib import Path

from ....config.weights import EDGE_WEIGHTS
from ....types import Fragment, FragmentId
from ...base import EdgeBuilder, EdgeDict
from ._parsing import (
    DISCOVERY_MAX_DEPTH,
    extract_definitions,
    extract_mods,
    extract_pub_uses,
    extract_references,
    extract_trait_impls,
    extract_uses,
    is_rust_file,
    read_cached,
    stem_to_mod_name,
)


class RustEdgeBuilder(EdgeBuilder):
    weight = 0.75
    mod_weight = EDGE_WEIGHTS["rust_mod"].forward
    use_weight = EDGE_WEIGHTS["rust_use"].forward
    type_weight = EDGE_WEIGHTS["rust_type"].forward
    fn_weight = EDGE_WEIGHTS["rust_fn"].forward
    same_crate_weight = EDGE_WEIGHTS["rust_same_crate"].forward
    reverse_weight_factor = EDGE_WEIGHTS["rust_mod"].reverse_factor

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        rust_changed = [f for f in changed_files if is_rust_file(f)]
        if not rust_changed:
            return []

        fc = kwargs.get("file_cache")
        cache: dict[Path, str] | None = fc if isinstance(fc, dict) else None

        rust_candidates = [f for f in all_candidate_files if is_rust_file(f)]

        mod_name_to_files: dict[str, list[Path]] = defaultdict(list)
        file_uses: dict[Path, set[str]] = {}
        file_mods: dict[Path, set[str]] = {}

        for candidate in rust_candidates:
            mod_name_to_files[stem_to_mod_name(candidate)].append(candidate)
            content = read_cached(candidate, cache)
            if content is None:
                continue
            file_uses[candidate] = extract_uses(content)
            file_mods[candidate] = extract_mods(content)

        changed_set = set(changed_files)
        discovered: set[Path] = set()
        frontier = set(rust_changed)

        for _depth in range(DISCOVERY_MAX_DEPTH):
            next_frontier = self._discover_one_hop(
                frontier, rust_candidates, changed_set, discovered, file_uses, file_mods, mod_name_to_files
            )
            discovered.update(next_frontier)
            frontier = next_frontier
            if not frontier:
                break

        return sorted(discovered)

    @staticmethod
    def _collect_forward_targets(
        frontier: set[Path], file_uses: dict[Path, set[str]], file_mods: dict[Path, set[str]]
    ) -> set[str]:
        targets: set[str] = set()
        for f in frontier:
            for use_path in file_uses.get(f, set()):
                for part in use_path.split("::"):
                    targets.add(part.lower())
            targets.update(m.lower() for m in file_mods.get(f, set()))
        return targets

    @staticmethod
    def _extract_use_parts(file_uses: dict[Path, set[str]], candidate: Path) -> set[str]:
        parts: set[str] = set()
        for use_path in file_uses.get(candidate, set()):
            for part in use_path.split("::"):
                parts.add(part.lower())
        return parts

    @staticmethod
    def _discover_one_hop(
        frontier: set[Path],
        candidates: list[Path],
        exclude: set[Path],
        already_found: set[Path],
        file_uses: dict[Path, set[str]],
        file_mods: dict[Path, set[str]],
        mod_name_to_files: dict[str, list[Path]],
    ) -> set[Path]:
        found: set[Path] = set()
        skip = exclude | already_found
        frontier_mod_names = {stem_to_mod_name(f) for f in frontier}
        forward_targets = RustEdgeBuilder._collect_forward_targets(frontier, file_uses, file_mods)

        for target_name in forward_targets:
            for candidate in mod_name_to_files.get(target_name, []):
                if candidate not in skip and candidate not in found:
                    found.add(candidate)

        for candidate in candidates:
            if candidate in skip or candidate in found:
                continue
            if (file_mods.get(candidate, set()) & frontier_mod_names) or (
                RustEdgeBuilder._extract_use_parts(file_uses, candidate) & frontier_mod_names
            ):
                found.add(candidate)

        return found

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        rust_frags = [f for f in fragments if is_rust_file(f.path)]
        if not rust_frags:
            return {}

        edges: EdgeDict = {}
        indices = self._build_indices(rust_frags)
        _name_to_frags, _mod_to_frags, type_defs, fn_defs, trait_impls = indices

        self._link_trait_impls(trait_impls, type_defs, edges)
        self._link_pub_use_edges(rust_frags, type_defs, fn_defs, edges)

        for rf in rust_frags:
            self._link_fragment(rf, rust_frags, indices, edges)

        return edges

    def _build_indices(self, rust_frags: list[Fragment]) -> tuple[
        dict[str, list[FragmentId]],
        dict[str, list[FragmentId]],
        dict[str, list[FragmentId]],
        dict[str, list[FragmentId]],
        dict[FragmentId, list[tuple[str, str]]],
    ]:
        name_to_frags: dict[str, list[FragmentId]] = defaultdict(list)
        mod_to_frags: dict[str, list[FragmentId]] = defaultdict(list)
        type_defs: dict[str, list[FragmentId]] = defaultdict(list)
        fn_defs: dict[str, list[FragmentId]] = defaultdict(list)
        trait_impls: dict[FragmentId, list[tuple[str, str]]] = defaultdict(list)

        for f in rust_frags:
            self._index_fragment(f, name_to_frags, mod_to_frags, type_defs, fn_defs, trait_impls)

        return name_to_frags, mod_to_frags, type_defs, fn_defs, trait_impls

    @staticmethod
    def _register_pub_use(
        leaf_lower: str,
        frag_id: FragmentId,
        name_to_frags: dict[str, list[FragmentId]],
        type_defs: dict[str, list[FragmentId]],
        fn_defs: dict[str, list[FragmentId]],
    ) -> None:
        if leaf_lower in name_to_frags:
            return
        for target_fid_list in (type_defs.get(leaf_lower, []), fn_defs.get(leaf_lower, [])):
            for target_fid in target_fid_list:
                if target_fid != frag_id:
                    name_to_frags[leaf_lower].append(frag_id)
                    return

    @staticmethod
    def _index_fragment(
        f: Fragment,
        name_to_frags: dict[str, list[FragmentId]],
        mod_to_frags: dict[str, list[FragmentId]],
        type_defs: dict[str, list[FragmentId]],
        fn_defs: dict[str, list[FragmentId]],
        trait_impls: dict[FragmentId, list[tuple[str, str]]],
    ) -> None:
        stem = f.path.stem.lower()
        name_to_frags[stem].append(f.id)

        if stem in {"mod", "lib"}:
            mod_to_frags[f.path.parent.name.lower()].append(f.id)
        else:
            mod_to_frags[stem].append(f.id)

        funcs, types = extract_definitions(f.content)
        for t in types:
            type_defs[t.lower()].append(f.id)
        for fn in funcs:
            fn_defs[fn.lower()].append(f.id)

        for mod_name in extract_mods(f.content):
            mod_to_frags[mod_name.lower()].append(f.id)

        for trait_name, type_name in extract_trait_impls(f.content):
            trait_impls[f.id].append((trait_name, type_name))

        for pub_use_path in extract_pub_uses(f.content):
            leaf_lower = pub_use_path.split("::")[-1].lower()
            RustEdgeBuilder._register_pub_use(leaf_lower, f.id, name_to_frags, type_defs, fn_defs)

    def _link_fragment(
        self,
        rf: Fragment,
        rust_frags: list[Fragment],
        indices: tuple[
            dict[str, list[FragmentId]],
            dict[str, list[FragmentId]],
            dict[str, list[FragmentId]],
            dict[str, list[FragmentId]],
            dict[FragmentId, list[tuple[str, str]]],
        ],
        edges: EdgeDict,
    ) -> None:
        name_to_frags, mod_to_frags, type_defs, fn_defs, _trait_impls = indices

        type_refs, fn_calls, path_calls = extract_references(rf.content)

        self._link_uses(rf, mod_to_frags, name_to_frags, edges)
        self._link_declared_mods(rf, name_to_frags, edges)
        self._link_refs(rf, type_refs, fn_calls, type_defs, fn_defs, edges)
        self._link_path_calls(rf, path_calls, mod_to_frags, edges)
        self._link_same_crate(rf, rust_frags, edges)

    def _link_trait_impls(
        self,
        trait_impls: dict[FragmentId, list[tuple[str, str]]],
        type_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for impl_fid, pairs in trait_impls.items():
            for trait_name, _type_name in pairs:
                for trait_fid in type_defs.get(trait_name.lower(), []):
                    if trait_fid != impl_fid:
                        self.add_edge(edges, impl_fid, trait_fid, self.type_weight)

    def _link_pub_use_edges(
        self,
        rust_frags: list[Fragment],
        type_defs: dict[str, list[FragmentId]],
        fn_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for f in rust_frags:
            for pub_use_path in extract_pub_uses(f.content):
                parts = pub_use_path.split("::")
                leaf_lower = parts[-1].lower()
                for target_fid_list in [type_defs.get(leaf_lower, []), fn_defs.get(leaf_lower, [])]:
                    for target_fid in target_fid_list:
                        if target_fid != f.id:
                            self.add_edge(edges, f.id, target_fid, self.use_weight)

    def _link_uses(
        self,
        rf: Fragment,
        mod_to_frags: dict[str, list[FragmentId]],
        name_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for use_path in extract_uses(rf.content):
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
        for mod_name in extract_mods(rf.content):
            for fid in name_to_frags.get(mod_name.lower(), []):
                if fid != rf.id:
                    self.add_edge(edges, rf.id, fid, self.mod_weight)

    def _link_refs(
        self,
        rf: Fragment,
        type_refs: set[str],
        fn_calls: set[str],
        type_defs: dict[str, list[FragmentId]],
        fn_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
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
        path_calls: set[tuple[str, str]],
        mod_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
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
        if rf.path.stem.lower() not in {"lib", "mod"}:
            return
        parent_dir = rf.path.parent
        for f in rust_frags:
            if f.path.parent == parent_dir and f.id != rf.id:
                self.add_edge(edges, rf.id, f.id, self.same_crate_weight)
