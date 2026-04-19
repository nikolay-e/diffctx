from __future__ import annotations

from collections import defaultdict
from pathlib import Path

from ....config.weights import EDGE_WEIGHTS
from ....types import Fragment, FragmentId
from ...base import EdgeBuilder, EdgeDict
from ._parsing import (
    _GO_EMBED_RE,
    _any_dir_matches,
    _extract_definitions,
    _extract_embedded_types,
    _extract_imports,
    _extract_references,
    _get_package_name_from_content,
    _has_init_func,
    _is_go_file,
    _resolve_bases,
)


class GoEdgeBuilder(EdgeBuilder):
    weight = 0.75
    import_weight = EDGE_WEIGHTS["go_import"].forward
    type_weight = EDGE_WEIGHTS["go_type"].forward
    func_weight = EDGE_WEIGHTS["go_func"].forward
    same_package_weight = EDGE_WEIGHTS["go_same_package"].forward
    init_same_package_weight = 0.15
    reverse_weight_factor = EDGE_WEIGHTS["go_import"].reverse_factor
    _DISCOVERY_MAX_DEPTH = 2

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        changed_set = set(changed_files)
        discovered: set[Path] = set()
        candidates = [c for c in all_candidate_files if c not in changed_set and _is_go_file(c)]

        go_changed = [f for f in changed_files if _is_go_file(f)]
        if go_changed:
            self._discover_same_package(go_changed, candidates, discovered)

        embed_go_files = self._discover_embed_files(changed_files, candidates, discovered, repo_root)
        self._discover_package_peers(embed_go_files, candidates, discovered)

        if go_changed:
            fc = kwargs.get("file_cache")
            file_cache: dict[Path, str] | None = fc if isinstance(fc, dict) else None
            candidate_index = self._build_candidate_import_index(candidates, file_cache)
            self._discover_cross_package(go_changed, changed_set, candidates, candidate_index, discovered, repo_root, file_cache)

        return sorted(discovered)

    def _build_candidate_import_index(
        self,
        candidates: list[Path],
        file_cache: dict[Path, str] | None,
    ) -> dict[Path, tuple[str, set[str]]]:
        index: dict[Path, tuple[str, set[str]]] = {}
        for c in candidates:
            content = self._read_file(c, file_cache)
            if content is None:
                continue
            pkg = _get_package_name_from_content(content, c).lower()
            imports = _extract_imports(content)
            index[c] = (pkg, imports)
        return index

    def _discover_cross_package(
        self,
        go_changed: list[Path],
        changed_set: set[Path],
        candidates: list[Path],
        candidate_index: dict[Path, tuple[str, set[str]]],
        discovered: set[Path],
        repo_root: Path | None,
        file_cache: dict[Path, str] | None,
    ) -> None:
        frontier = set(go_changed)
        for _depth in range(self._DISCOVERY_MAX_DEPTH):
            next_frontier: set[Path] = set()
            for f in frontier:
                content = self._read_file(f, file_cache)
                if content is None:
                    continue
                f_imports = _extract_imports(content)
                f_pkg = _get_package_name_from_content(content, f).lower()
                f_rel_dir = str(f.relative_to(repo_root).parent) if repo_root else None
                self._match_candidates(
                    candidates,
                    changed_set,
                    discovered,
                    next_frontier,
                    candidate_index,
                    f_imports,
                    f,
                    f_pkg,
                    f_rel_dir,
                    repo_root,
                )
            frontier = next_frontier
            if not frontier:
                break

    def _match_candidates(
        self,
        candidates: list[Path],
        changed_set: set[Path],
        discovered: set[Path],
        next_frontier: set[Path],
        candidate_index: dict[Path, tuple[str, set[str]]],
        f_imports: set[str],
        f: Path,
        f_pkg: str,
        f_rel_dir: str | None,
        repo_root: Path | None,
    ) -> None:
        for c in candidates:
            if c in changed_set or c in discovered:
                continue
            c_info = candidate_index.get(c)
            if c_info is None:
                continue
            c_pkg, c_imports = c_info
            if self._import_matches_file(f_imports, c, c_pkg, repo_root) or self._import_matches_source(
                c_imports, f, f_pkg, f_rel_dir, repo_root
            ):
                discovered.add(c)
                next_frontier.add(c)

    @staticmethod
    def _import_matches_file(
        imports: set[str],
        candidate: Path,
        candidate_pkg: str,
        repo_root: Path | None,
    ) -> bool:
        for imp in imports:
            imp_pkg = imp.split("/")[-1].lower()
            if imp_pkg == candidate_pkg:
                return True
            if repo_root:
                try:
                    rel = str(candidate.relative_to(repo_root).parent)
                    if imp == rel or imp.endswith(f"/{rel}") or f"/{rel}/" in imp:
                        return True
                except ValueError:
                    pass
        return False

    @staticmethod
    def _import_matches_source(
        candidate_imports: set[str],
        source: Path,
        source_pkg: str,
        source_rel_dir: str | None,
        repo_root: Path | None,
    ) -> bool:
        for imp in candidate_imports:
            imp_pkg = imp.split("/")[-1].lower()
            if imp_pkg == source_pkg:
                return True
            if source_rel_dir is not None:
                if imp == source_rel_dir or imp.endswith(f"/{source_rel_dir}") or f"/{source_rel_dir}/" in imp:
                    return True
        return False

    def _discover_same_package(self, go_changed: list[Path], candidates: list[Path], discovered: set[Path]) -> None:
        pkg_dirs = {f.parent for f in go_changed}
        for c in candidates:
            if c.parent in pkg_dirs:
                discovered.add(c)

    def _discover_embed_files(
        self,
        changed_files: list[Path],
        candidates: list[Path],
        discovered: set[Path],
        repo_root: Path | None,
    ) -> set[Path]:
        changed_dirs = {f.parent for f in changed_files}
        embed_go_files: set[Path] = set()
        for c in candidates:
            if self._embeds_any_changed_dir(c, changed_dirs, repo_root):
                discovered.add(c)
                embed_go_files.add(c)
        return embed_go_files

    def _discover_package_peers(self, embed_go_files: set[Path], candidates: list[Path], discovered: set[Path]) -> None:
        embed_dirs = {f.parent for f in embed_go_files}
        for c in candidates:
            if c not in discovered and c.parent in embed_dirs:
                discovered.add(c)

    def _embeds_any_changed_dir(self, go_file: Path, changed_dirs: set[Path], repo_root: Path | None = None) -> bool:
        try:
            content = go_file.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            return False
        for match in _GO_EMBED_RE.finditer(content):
            embed_dirs = _resolve_bases(match.group(1), go_file.parent, repo_root)
            if _any_dir_matches(changed_dirs, embed_dirs):
                return True
        return False

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        go_frags = [f for f in fragments if _is_go_file(f.path)]
        if not go_frags:
            return {}

        edges: EdgeDict = {}
        indices = self._build_indices(go_frags, repo_root)

        for gf in go_frags:
            self._link_fragment(gf, indices, edges)

        self._build_embed_edges(go_frags, fragments, edges, repo_root)
        return edges

    def _build_embed_edges(
        self,
        go_frags: list[Fragment],
        all_frags: list[Fragment],
        edges: EdgeDict,
        repo_root: Path | None = None,
    ) -> None:
        non_go_frags = [f for f in all_frags if not _is_go_file(f.path)]
        for gf in go_frags:
            for match in _GO_EMBED_RE.finditer(gf.content):
                embed_dirs = _resolve_bases(match.group(1), gf.path.parent, repo_root)
                self._link_embed_targets(gf, non_go_frags, embed_dirs, edges)

    def _link_embed_targets(
        self,
        gf: Fragment,
        non_go_frags: list[Fragment],
        embed_dirs: list[Path],
        edges: EdgeDict,
    ) -> None:
        for frag in non_go_frags:
            try:
                frag_resolved = frag.path.resolve()
                if any(frag_resolved.is_relative_to(ed) for ed in embed_dirs):
                    self.add_edge(edges, gf.id, frag.id, self.weight * 0.8)
            except (ValueError, OSError):
                continue

    def _build_indices(
        self, go_frags: list[Fragment], repo_root: Path | None
    ) -> tuple[
        dict[str, list[FragmentId]], dict[str, list[FragmentId]], dict[str, list[FragmentId]], dict[str, list[FragmentId]]
    ]:
        pkg_to_frags: dict[str, list[FragmentId]] = defaultdict(list)
        path_to_frags: dict[str, list[FragmentId]] = defaultdict(list)
        type_defs: dict[str, list[FragmentId]] = defaultdict(list)
        func_defs: dict[str, list[FragmentId]] = defaultdict(list)

        for f in go_frags:
            pkg = _get_package_name_from_content(f.content, f.path).lower()
            pkg_to_frags[pkg].append(f.id)

            if repo_root:
                try:
                    rel = f.path.relative_to(repo_root)
                    path_to_frags[str(rel.parent)].append(f.id)
                except ValueError:
                    pass

            funcs, types = _extract_definitions(f.content)
            for t in types:
                type_defs[t.lower()].append(f.id)
            for fn in funcs:
                func_defs[fn.lower()].append(f.id)

        return pkg_to_frags, path_to_frags, type_defs, func_defs

    def _link_fragment(
        self,
        gf: Fragment,
        indices: tuple[
            dict[str, list[FragmentId]], dict[str, list[FragmentId]], dict[str, list[FragmentId]], dict[str, list[FragmentId]]
        ],
        edges: EdgeDict,
    ) -> None:
        pkg_to_frags, path_to_frags, type_defs, func_defs = indices
        imports = _extract_imports(gf.content)
        func_calls, type_refs, pkg_calls = _extract_references(gf.content)

        self._link_imports(gf, imports, pkg_to_frags, path_to_frags, edges)
        self._link_refs(gf, type_refs, type_defs, self.type_weight, edges)
        self._link_refs(gf, func_calls, func_defs, self.func_weight, edges)
        self._link_pkg_calls(gf, pkg_calls, pkg_to_frags, edges)
        self._link_embedded_types(gf, type_defs, edges)

        has_init = _has_init_func(gf.content)
        self._link_same_package(gf, pkg_to_frags, edges, has_init)

    def _link_imports(
        self,
        gf: Fragment,
        imports: set[str],
        pkg_to_frags: dict[str, list[FragmentId]],
        path_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for imp in imports:
            self._link_import_by_package(gf.id, imp, pkg_to_frags, edges)
            self._link_import_by_path(gf.id, imp, path_to_frags, edges)

    def _link_import_by_package(
        self,
        gf_id: FragmentId,
        imp: str,
        pkg_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        imp_pkg = imp.split("/")[-1].lower()
        for pkg, frag_ids in pkg_to_frags.items():
            if pkg == imp_pkg:
                self.add_edges_from_ids(gf_id, frag_ids, self.import_weight, edges)

    def _link_import_by_path(
        self,
        gf_id: FragmentId,
        imp: str,
        path_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for path_str, frag_ids in path_to_frags.items():
            if imp == path_str or imp.endswith(f"/{path_str}") or f"/{path_str}/" in imp:
                self.add_edges_from_ids(gf_id, frag_ids, self.import_weight, edges)

    def _link_refs(
        self,
        gf: Fragment,
        refs: set[str],
        defs: dict[str, list[FragmentId]],
        weight: float,
        edges: EdgeDict,
    ) -> None:
        for ref in refs:
            for fid in defs.get(ref.lower(), []):
                if fid != gf.id:
                    self.add_edge(edges, gf.id, fid, weight)

    def _link_pkg_calls(
        self,
        gf: Fragment,
        pkg_calls: set[tuple[str, str]],
        pkg_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for pkg_name, _symbol in pkg_calls:
            for fid in pkg_to_frags.get(pkg_name.lower(), []):
                if fid != gf.id:
                    self.add_edge(edges, gf.id, fid, self.func_weight)

    def _link_embedded_types(
        self,
        gf: Fragment,
        type_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        embedded_map = _extract_embedded_types(gf.content)
        for embeds in embedded_map.values():
            for embed_name in embeds:
                for fid in type_defs.get(embed_name.lower(), []):
                    if fid != gf.id:
                        self.add_edge(edges, gf.id, fid, self.type_weight)

    def _link_same_package(
        self,
        gf: Fragment,
        pkg_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
        has_init: bool = False,
    ) -> None:
        weight = self.init_same_package_weight if has_init else self.same_package_weight
        current_pkg = _get_package_name_from_content(gf.content, gf.path).lower()
        for fid in pkg_to_frags.get(current_pkg, []):
            if fid != gf.id:
                self.add_edge(edges, gf.id, fid, weight)
