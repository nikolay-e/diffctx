# pylint: disable=duplicate-code
from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import LANG_WEIGHTS
from ...python_semantics import PyFragmentInfo, analyze_python_fragment
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, add_semantic_edges, path_to_module

_PYTHON_EXTS = {".py", ".pyi", ".pyw"}

_PY_WEIGHTS = LANG_WEIGHTS["python"]
_CALL_WEIGHT = _PY_WEIGHTS.call
_SYMBOL_REF_WEIGHT = _PY_WEIGHTS.symbol_ref
_TYPE_REF_WEIGHT = _PY_WEIGHTS.type_ref

_PY_IMPORT_RE = re.compile(r"(?:from\s{1,20}(\.{0,3}[\w.]{0,200})\s{1,20}import|import\s{1,20}([\w.]{1,200}))")


def _is_python_file(path: Path) -> bool:
    return path.suffix.lower() in _PYTHON_EXTS


def _count_leading_dots(s: str) -> int:
    return len(s) - len(s.lstrip("."))


def _strip_source_prefix(parts: list[str]) -> list[str]:
    for i, part in enumerate(parts):
        if part in ("src", "lib", "packages"):
            return parts[i + 1 :]
    return parts


def _resolve_relative_import(imported: str, source_path: Path, repo_root: Path | None = None) -> str | None:
    if not imported.startswith("."):
        return imported

    dots = _count_leading_dots(imported)
    relative_module = imported[dots:]

    if repo_root and source_path.is_absolute():
        try:
            source_path = source_path.relative_to(repo_root)
        except ValueError:
            pass

    parent_parts = _strip_source_prefix(list(source_path.parent.parts))

    if parent_parts and parent_parts[-1] == "__pycache__":
        parent_parts = parent_parts[:-1]

    for _ in range(dots - 1):
        if parent_parts:
            parent_parts.pop()

    if relative_module:
        parent_parts.extend(relative_module.split("."))

    return ".".join(parent_parts) if parent_parts else None


def _extract_imports_from_content(content: str, source_path: Path | None = None, repo_root: Path | None = None) -> set[str]:
    imports: set[str] = set()
    for match in _PY_IMPORT_RE.finditer(content):
        imported = match.group(1) or match.group(2)
        if not imported:
            continue

        if imported.startswith(".") and source_path:
            resolved = _resolve_relative_import(imported, source_path, repo_root)
            if resolved:
                imported = resolved
            else:
                continue

        imports.add(imported)
        parts = imported.split(".")
        for i in range(1, len(parts) + 1):
            imports.add(".".join(parts[:i]))
    return imports


class PythonEdgeBuilder(EdgeBuilder):
    weight = 0.70
    reverse_weight_factor = 0.5

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        py_changed = [f for f in changed_files if _is_python_file(f)]
        if not py_changed:
            return []

        changed_set = set(changed_files)
        discovered: set[Path] = set()

        # Forward dependencies: files imported BY changed files
        imported_modules = self._collect_imported_modules(py_changed, repo_root)
        if imported_modules:
            discovered.update(self._find_files_for_modules(all_candidate_files, changed_set, imported_modules, repo_root))

        # Backward dependencies: files that import the changed modules
        changed_modules = self._collect_changed_modules(py_changed, repo_root)
        if changed_modules:
            discovered.update(self._find_files_importing_modules(all_candidate_files, changed_set, changed_modules))

        return list(discovered)

    def _collect_imported_modules(self, py_changed: list[Path], repo_root: Path | None = None) -> set[str]:
        imported_modules: set[str] = set()
        for f in py_changed:
            try:
                content = f.read_text(encoding="utf-8")
                imports = _extract_imports_from_content(content, f, repo_root)
                imported_modules.update(imports)
            except (OSError, UnicodeDecodeError):
                continue
        return imported_modules

    def _find_files_for_modules(
        self, all_candidate_files: list[Path], changed_set: set[Path], modules: set[str], repo_root: Path | None
    ) -> list[Path]:
        discovered: list[Path] = []
        for candidate in all_candidate_files:
            if candidate in changed_set or not _is_python_file(candidate):
                continue
            module = path_to_module(candidate, repo_root)
            if module and module in modules:
                discovered.append(candidate)
        return discovered

    def _collect_changed_modules(self, py_changed: list[Path], repo_root: Path | None) -> set[str]:
        changed_modules: set[str] = set()
        for f in py_changed:
            module = path_to_module(f, repo_root)
            if module:
                changed_modules.add(module)
                parts = module.split(".")
                for i in range(1, len(parts) + 1):
                    changed_modules.add(".".join(parts[:i]))
        return changed_modules

    def _find_files_importing_modules(
        self, all_candidate_files: list[Path], changed_set: set[Path], changed_modules: set[str]
    ) -> list[Path]:
        discovered: list[Path] = []
        for candidate in all_candidate_files:
            if candidate in changed_set or not _is_python_file(candidate):
                continue
            if self._imports_any_module(candidate, changed_modules):
                discovered.append(candidate)
        return discovered

    def _imports_any_module(self, candidate: Path, changed_modules: set[str]) -> bool:
        try:
            content = candidate.read_text(encoding="utf-8")
            imports = _extract_imports_from_content(content, candidate)
            return any(imp in changed_modules for imp in imports)
        except (OSError, UnicodeDecodeError):
            return False

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

        edges: EdgeDict = {}

        for f in py_frags:
            info = info_cache[f.id]
            self_defs = set(frag_defines.get(f.id, frozenset()))

            add_semantic_edges(
                edges,
                f.id,
                info,
                name_to_defs,
                _CALL_WEIGHT,
                _SYMBOL_REF_WEIGHT,
                _TYPE_REF_WEIGHT,
                self.reverse_weight_factor,
                self_defs,
            )

        return edges
