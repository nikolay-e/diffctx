# pylint: disable=duplicate-code
from __future__ import annotations

import logging
import subprocess
from collections import defaultdict
from pathlib import Path

from ....config.weights import LANG_WEIGHTS
from ....types import Fragment, FragmentId
from ...base import EdgeBuilder, EdgeDict, path_to_module
from ..python_semantics import PyFragmentInfo, analyze_python_fragment
from ._parsing import _INIT_PY, _extract_imports_from_content, _is_python_file
from ._reexports import _resolve_init_reexports
from ._ripgrep import _RG_BIN, _build_import_index_rg

logger = logging.getLogger(__name__)

_PY_WEIGHTS = LANG_WEIGHTS["python"]
_CALL_WEIGHT = _PY_WEIGHTS.call
_SYMBOL_REF_WEIGHT = _PY_WEIGHTS.symbol_ref
_TYPE_REF_WEIGHT = _PY_WEIGHTS.type_ref


class PythonEdgeBuilder(EdgeBuilder):
    weight = 0.70
    reverse_weight_factor = 0.5

    _DISCOVERY_MAX_DEPTH = 2

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        py_changed = [f for f in changed_files if _is_python_file(f)]
        if not py_changed:
            return []

        fc = kwargs.get("file_cache")
        cache: dict[Path, str] | None = fc if isinstance(fc, dict) else None
        file_to_module, module_to_files, file_to_imports = self._build_import_index(all_candidate_files, repo_root, cache)

        changed_set = set(changed_files)
        discovered: set[Path] = set()
        frontier = set(py_changed)

        for _depth in range(self._DISCOVERY_MAX_DEPTH):
            next_frontier = self._expand_frontier(
                frontier,
                changed_set,
                discovered,
                file_to_imports,
                file_to_module,
                module_to_files,
                repo_root,
            )
            frontier = next_frontier
            if not frontier:
                break

        return sorted(discovered)

    @staticmethod
    def _add_forward_imports(
        f_imports: set[str],
        module_to_files: dict[str, list[Path]],
        changed_set: set[Path],
        discovered: set[Path],
        next_frontier: set[Path],
    ) -> None:
        for imp in f_imports:
            for target in module_to_files.get(imp, []):
                if target not in changed_set and target not in discovered:
                    discovered.add(target)
                    next_frontier.add(target)

    @staticmethod
    def _add_reverse_imports(
        f_module: str,
        file_to_imports: dict[Path, set[str]],
        changed_set: set[Path],
        discovered: set[Path],
        next_frontier: set[Path],
    ) -> None:
        for candidate, cand_imports in file_to_imports.items():
            if candidate not in changed_set and candidate not in discovered and f_module in cand_imports:
                discovered.add(candidate)
                next_frontier.add(candidate)

    @staticmethod
    def _expand_frontier(
        frontier: set[Path],
        changed_set: set[Path],
        discovered: set[Path],
        file_to_imports: dict[Path, set[str]],
        file_to_module: dict[Path, str],
        module_to_files: dict[str, list[Path]],
        repo_root: Path | None,
    ) -> set[Path]:
        next_frontier: set[Path] = set()
        for f in frontier:
            f_imports = file_to_imports.get(f)
            if f_imports is None:
                try:
                    content = f.read_text(encoding="utf-8")
                    f_imports = _extract_imports_from_content(content, f, repo_root)
                except (OSError, UnicodeDecodeError):
                    continue
            PythonEdgeBuilder._add_forward_imports(f_imports, module_to_files, changed_set, discovered, next_frontier)
            f_module = file_to_module.get(f) or path_to_module(f, repo_root)
            if f_module:
                PythonEdgeBuilder._add_reverse_imports(f_module, file_to_imports, changed_set, discovered, next_frontier)
        return next_frontier

    @staticmethod
    def _build_module_index(
        all_candidate_files: list[Path],
        repo_root: Path | None,
    ) -> tuple[dict[Path, str], dict[str, list[Path]]]:
        file_to_module: dict[Path, str] = {}
        module_to_files: dict[str, list[Path]] = defaultdict(list)
        for f in all_candidate_files:
            if not _is_python_file(f):
                continue
            module = path_to_module(f, repo_root)
            if module:
                file_to_module[f] = module
                module_to_files[module].append(f)
                parts = module.split(".")
                for i in range(1, len(parts)):
                    module_to_files[".".join(parts[:i])].append(f)
        return file_to_module, module_to_files

    @staticmethod
    def _build_import_index_ast(
        all_candidate_files: list[Path],
        repo_root: Path | None,
        file_cache: dict[Path, str] | None,
    ) -> dict[Path, set[str]]:
        file_to_imports: dict[Path, set[str]] = {}
        for f in all_candidate_files:
            if not _is_python_file(f):
                continue
            content = file_cache.get(f) if file_cache else None
            if content is None:
                try:
                    content = f.read_text(encoding="utf-8")
                except (OSError, UnicodeDecodeError):
                    continue
            file_to_imports[f] = _extract_imports_from_content(content, f, repo_root)
        return file_to_imports

    def _build_import_index(
        self,
        all_candidate_files: list[Path],
        repo_root: Path | None,
        file_cache: dict[Path, str] | None = None,
    ) -> tuple[dict[Path, str], dict[str, list[Path]], dict[Path, set[str]]]:
        file_to_module, module_to_files = self._build_module_index(all_candidate_files, repo_root)
        self._enrich_with_reexports(all_candidate_files, repo_root, file_cache, file_to_module, module_to_files)

        if _RG_BIN and repo_root:
            candidate_set = {f for f in all_candidate_files if _is_python_file(f)}
            try:
                file_to_imports = _build_import_index_rg(repo_root, candidate_set)
                return file_to_module, dict(module_to_files), dict(file_to_imports)
            except (subprocess.TimeoutExpired, OSError):
                logger.debug("ripgrep import index failed, falling back to ast")

        return file_to_module, dict(module_to_files), self._build_import_index_ast(all_candidate_files, repo_root, file_cache)

    @staticmethod
    def _register_reexport_source(
        source_path: Path,
        pkg_module: str,
        repo_root: Path | None,
        module_to_files: dict[str, list[Path]],
        file_to_module: dict[Path, str],
        existing: set[Path],
    ) -> None:
        if source_path not in existing:
            module_to_files[pkg_module].append(source_path)
            existing.add(source_path)
            if source_path not in file_to_module:
                src_module = path_to_module(source_path, repo_root)
                if src_module:
                    file_to_module[source_path] = src_module

    @staticmethod
    def _enrich_with_reexports(
        all_candidate_files: list[Path],
        repo_root: Path | None,
        file_cache: dict[Path, str] | None,
        file_to_module: dict[Path, str],
        module_to_files: dict[str, list[Path]],
    ) -> None:
        for init_f in (f for f in all_candidate_files if f.name == _INIT_PY):
            pkg_module = path_to_module(init_f, repo_root)
            if not pkg_module:
                continue
            reexports = _resolve_init_reexports(init_f, repo_root, file_cache)
            existing = set(module_to_files.get(pkg_module, []))
            for source_path in reexports.values():
                PythonEdgeBuilder._register_reexport_source(
                    source_path, pkg_module, repo_root, module_to_files, file_to_module, existing
                )

    @staticmethod
    def _enrich_frags_with_reexports(
        py_frags: list[Fragment],
        repo_root: Path | None,
        file_cache: dict[Path, str],
        module_to_frags: dict[str, list[FragmentId]],
        path_to_frags: dict[Path, list[FragmentId]],
    ) -> None:
        for f in py_frags:
            if f.path.name != _INIT_PY:
                continue
            pkg_module = path_to_module(f.path, repo_root)
            if not pkg_module:
                continue
            reexports = _resolve_init_reexports(f.path, repo_root, file_cache)
            existing = set(module_to_frags.get(pkg_module, []))
            for _name, source_path in reexports.items():
                for sf in path_to_frags.get(source_path, []):
                    if sf not in existing:
                        module_to_frags[pkg_module].append(sf)

    @staticmethod
    def _build_frag_indexes(
        py_frags: list[Fragment],
        info_cache: dict[FragmentId, PyFragmentInfo],
        repo_root: Path | None,
    ) -> tuple[
        dict[str, list[FragmentId]], dict[FragmentId, frozenset[str]], dict[str, list[FragmentId]], dict[Path, list[FragmentId]]
    ]:
        name_to_defs: dict[str, list[FragmentId]] = defaultdict(list)
        frag_defines: dict[FragmentId, frozenset[str]] = {}
        for f in py_frags:
            info = info_cache[f.id]
            frag_defines[f.id] = info.defines
            for name in info.defines:
                name_to_defs[name].append(f.id)

        module_to_frags: dict[str, list[FragmentId]] = defaultdict(list)
        path_to_frags: dict[Path, list[FragmentId]] = defaultdict(list)
        for f in py_frags:
            module = path_to_module(f.path, repo_root)
            if module:
                module_to_frags[module].append(f.id)
            path_to_frags[f.path].append(f.id)

        return name_to_defs, frag_defines, module_to_frags, path_to_frags

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        py_frags = [f for f in fragments if _is_python_file(f.path)]
        if not py_frags:
            return {}

        info_cache: dict[FragmentId, PyFragmentInfo] = {}
        for f in py_frags:
            info_cache[f.id] = analyze_python_fragment(f.content)

        name_to_defs, frag_defines, module_to_frags, path_to_frags = self._build_frag_indexes(
            py_frags,
            info_cache,
            repo_root,
        )

        file_cache: dict[Path, str] = {f.path: f.content for f in py_frags}
        self._enrich_frags_with_reexports(py_frags, repo_root, file_cache, module_to_frags, path_to_frags)

        frag_imports = {f.id: _extract_imports_from_content(f.content, f.path, repo_root) for f in py_frags}
        frag_to_module = {f.id: m for f in py_frags if (m := path_to_module(f.path, repo_root))}

        edges: EdgeDict = {}
        for f in py_frags:
            info = info_cache[f.id]
            self_defs = set(frag_defines.get(f.id, frozenset()))
            src_imports = frag_imports.get(f.id, set())
            self._add_import_confirmed_edges(edges, f.id, info, name_to_defs, self_defs, src_imports, frag_to_module)
            self._add_import_edges(f, frag_imports[f.id], module_to_frags, edges)

        return edges

    _IMPORT_CONFIRMED_BOOST = 1.5
    _IMPORT_UNCONFIRMED_PENALTY = 0.2
    _IMPORT_WEIGHT = 0.75

    def _add_import_confirmed_edges(
        self,
        edges: EdgeDict,
        src_id: FragmentId,
        info: PyFragmentInfo,
        name_to_defs: dict[str, list[FragmentId]],
        self_defs: set[str],
        src_imports: set[str],
        frag_to_module: dict[FragmentId, str],
    ) -> None:
        for ref_set, base_weight in [
            (info.calls, _CALL_WEIGHT),
            (info.references, _SYMBOL_REF_WEIGHT),
            (info.type_refs, _TYPE_REF_WEIGHT),
        ]:
            for name in ref_set:
                if name in self_defs:
                    continue
                for dst_id in name_to_defs.get(name, []):
                    if dst_id == src_id:
                        continue
                    dst_module = frag_to_module.get(dst_id, "")
                    confirmed = bool(dst_module and dst_module in src_imports)
                    factor = self._IMPORT_CONFIRMED_BOOST if confirmed else self._IMPORT_UNCONFIRMED_PENALTY
                    w = base_weight * factor
                    edges[(src_id, dst_id)] = max(edges.get((src_id, dst_id), 0.0), w)
                    rev_w = w * self.reverse_weight_factor
                    edges[(dst_id, src_id)] = max(edges.get((dst_id, src_id), 0.0), rev_w)

    def _add_import_edges(
        self,
        frag: Fragment,
        imports: set[str],
        module_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for imp in imports:
            targets = module_to_frags.get(imp, [])
            for tgt in targets:
                if tgt == frag.id:
                    continue
                edges[(frag.id, tgt)] = max(edges.get((frag.id, tgt), 0.0), self._IMPORT_WEIGHT)
