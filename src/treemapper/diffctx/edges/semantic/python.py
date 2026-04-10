# pylint: disable=duplicate-code
from __future__ import annotations

import ast
from collections import defaultdict
from pathlib import Path

from ...config.weights import LANG_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, _strip_source_prefix, path_to_module
from .python_semantics import PyFragmentInfo, analyze_python_fragment

_PYTHON_EXTS = {".py", ".pyi", ".pyw"}

_PY_WEIGHTS = LANG_WEIGHTS["python"]
_CALL_WEIGHT = _PY_WEIGHTS.call
_SYMBOL_REF_WEIGHT = _PY_WEIGHTS.symbol_ref
_TYPE_REF_WEIGHT = _PY_WEIGHTS.type_ref


def _is_python_file(path: Path) -> bool:
    return path.suffix.lower() in _PYTHON_EXTS


def _add_import_with_prefixes(imports: set[str], imported: str) -> None:
    imports.add(imported)
    parts = imported.split(".")
    for i in range(1, len(parts) + 1):
        imports.add(".".join(parts[:i]))


def _resolve_relative(name: str, source_path: Path, repo_root: Path | None) -> str | None:
    try:
        import importlib.util

        pkg_parts = _strip_source_prefix(list(source_path.parent.parts))
        if pkg_parts and pkg_parts[-1] == "__pycache__":
            pkg_parts = pkg_parts[:-1]
        if repo_root and source_path.is_absolute():
            try:
                source_path = source_path.relative_to(repo_root)
                pkg_parts = _strip_source_prefix(list(source_path.parent.parts))
            except ValueError:
                pass
        package = ".".join(pkg_parts) if pkg_parts else None
        if not package:
            return None
        return importlib.util.resolve_name(name, package)
    except (ImportError, ValueError):
        return None


def _extract_imports_from_content(content: str, source_path: Path | None = None, repo_root: Path | None = None) -> set[str]:
    imports: set[str] = set()
    try:
        tree = ast.parse(content)
    except SyntaxError:
        return imports

    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                if alias.name:
                    _add_import_with_prefixes(imports, alias.name)
        elif isinstance(node, ast.ImportFrom):
            module = node.module or ""
            if node.level and node.level > 0:
                dots = "." * node.level
                relative = dots + module
                if source_path:
                    resolved = _resolve_relative(relative, source_path, repo_root)
                    if resolved:
                        _add_import_with_prefixes(imports, resolved)
            elif module:
                _add_import_with_prefixes(imports, module)
    return imports


class PythonEdgeBuilder(EdgeBuilder):
    weight = 0.70
    reverse_weight_factor = 0.5

    _DISCOVERY_MAX_DEPTH = 2

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        py_changed = [f for f in changed_files if _is_python_file(f)]
        if not py_changed:
            return []

        file_to_module, module_to_files, file_to_imports = self._build_import_index(all_candidate_files, repo_root)

        changed_set = set(changed_files)
        discovered: set[Path] = set()
        frontier = set(py_changed)

        for _depth in range(self._DISCOVERY_MAX_DEPTH):
            next_frontier: set[Path] = set()
            for f in frontier:
                f_imports = file_to_imports.get(f)
                if f_imports is None:
                    try:
                        content = f.read_text(encoding="utf-8")
                        f_imports = _extract_imports_from_content(content, f, repo_root)
                    except (OSError, UnicodeDecodeError):
                        continue

                for imp in f_imports:
                    for target in module_to_files.get(imp, []):
                        if target not in changed_set and target not in discovered:
                            discovered.add(target)
                            next_frontier.add(target)

                f_module = file_to_module.get(f) or path_to_module(f, repo_root)
                if f_module:
                    for candidate, cand_imports in file_to_imports.items():
                        if candidate in changed_set or candidate in discovered:
                            continue
                        if f_module in cand_imports:
                            discovered.add(candidate)
                            next_frontier.add(candidate)

            frontier = next_frontier
            if not frontier:
                break

        return list(discovered)

    def _build_import_index(
        self, all_candidate_files: list[Path], repo_root: Path | None
    ) -> tuple[dict[Path, str], dict[str, list[Path]], dict[Path, set[str]]]:
        file_to_module: dict[Path, str] = {}
        module_to_files: dict[str, list[Path]] = defaultdict(list)
        file_to_imports: dict[Path, set[str]] = {}

        for f in all_candidate_files:
            if not _is_python_file(f):
                continue
            module = path_to_module(f, repo_root)
            if module:
                file_to_module[f] = module
                module_to_files[module].append(f)
                parts = module.split(".")
                for i in range(1, len(parts)):
                    prefix = ".".join(parts[:i])
                    module_to_files[prefix].append(f)
            try:
                content = f.read_text(encoding="utf-8")
                file_to_imports[f] = _extract_imports_from_content(content, f, repo_root)
            except (OSError, UnicodeDecodeError):
                continue

        return file_to_module, dict(module_to_files), file_to_imports

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        py_frags = [f for f in fragments if _is_python_file(f.path)]
        if not py_frags:
            return {}

        info_cache: dict[FragmentId, PyFragmentInfo] = {}
        for f in py_frags:
            info_cache[f.id] = analyze_python_fragment(f.content)

        name_to_defs: dict[str, list[FragmentId]] = defaultdict(list)
        frag_defines: dict[FragmentId, frozenset[str]] = {}

        for f in py_frags:
            info = info_cache[f.id]
            frag_defines[f.id] = info.defines
            for name in info.defines:
                name_to_defs[name].append(f.id)

        module_to_frags: dict[str, list[FragmentId]] = defaultdict(list)
        for f in py_frags:
            module = path_to_module(f.path, repo_root)
            if module:
                module_to_frags[module].append(f.id)

        frag_imports: dict[FragmentId, set[str]] = {}
        for f in py_frags:
            frag_imports[f.id] = _extract_imports_from_content(f.content, f.path, repo_root)

        frag_to_module: dict[FragmentId, str] = {}
        for f in py_frags:
            m = path_to_module(f.path, repo_root)
            if m:
                frag_to_module[f.id] = m

        edges: EdgeDict = {}

        for f in py_frags:
            info = info_cache[f.id]
            self_defs = set(frag_defines.get(f.id, frozenset()))
            src_imports = frag_imports.get(f.id, set())

            self._add_import_confirmed_edges(
                edges,
                f.id,
                info,
                name_to_defs,
                self_defs,
                src_imports,
                frag_to_module,
            )

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
