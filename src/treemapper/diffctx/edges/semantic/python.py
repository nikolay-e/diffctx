# pylint: disable=duplicate-code
from __future__ import annotations

import ast
import logging
import os
import re
import shutil
import subprocess
from collections import defaultdict
from pathlib import Path

from ...config.weights import LANG_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, _strip_source_prefix, path_to_module
from .python_semantics import PyFragmentInfo, analyze_python_fragment

logger = logging.getLogger(__name__)

_RG_BIN = None if os.environ.get("DIFFCTX_NO_RIPGREP") else shutil.which("rg")

_IMPORT_FROM_RE = re.compile(r"^\s*from\s+(\.+)?([\w.]*)\s+import")
_IMPORT_SIMPLE_RE = re.compile(r"^\s*import\s+([\w.]+(?:\s*,\s*[\w.]+)*)")

_INIT_PY = "__init__.py"
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


def _collect_import_from(
    node: ast.ImportFrom,
    imports: set[str],
    source_path: Path | None,
    repo_root: Path | None,
) -> None:
    module = node.module or ""
    if node.level and node.level > 0:
        relative = "." * node.level + module
        if source_path:
            resolved = _resolve_relative(relative, source_path, repo_root)
            if resolved:
                _add_import_with_prefixes(imports, resolved)
    elif module:
        _add_import_with_prefixes(imports, module)
        for alias in node.names:
            if alias.name and alias.name != "*":
                _add_import_with_prefixes(imports, f"{module}.{alias.name}")


def _extract_imports_from_content(content: str, source_path: Path | None = None, repo_root: Path | None = None) -> set[str]:
    imports: set[str] = set()
    import warnings

    try:
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", SyntaxWarning)
            tree = ast.parse(content)
    except SyntaxError:
        return imports

    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                if alias.name:
                    _add_import_with_prefixes(imports, alias.name)
        elif isinstance(node, ast.ImportFrom):
            _collect_import_from(node, imports, source_path, repo_root)
    return imports


def _resolve_relative_rg(source_path: Path, repo_root: Path, level: int, module: str) -> str | None:
    try:
        rel = source_path.relative_to(repo_root)
    except ValueError:
        return None
    parts = _strip_source_prefix(list(rel.parent.parts))
    if parts and parts[-1] == "__pycache__":
        parts = parts[:-1]
    if level > len(parts):
        return None
    base_parts = parts[: len(parts) - level + 1]
    if module:
        base_parts.extend(module.split("."))
    return ".".join(base_parts) if base_parts else None


def _build_import_index_rg(
    repo_root: Path,
    candidate_set: set[Path],
) -> dict[Path, set[str]]:
    r = subprocess.run(
        [
            _RG_BIN or "rg",
            "--no-heading",
            "--with-filename",
            r"^\s*(?:from\s+\.*[\w.]*\s+import|import\s+[\w.])",
            "--type",
            "py",
            str(repo_root),
        ],
        capture_output=True,
        text=True,
        timeout=30,
    )
    file_to_imports: dict[Path, set[str]] = defaultdict(set)
    for line in r.stdout.splitlines():
        idx = line.find(":")
        if idx < 0:
            continue
        path_str = line[:idx]
        rest = line[idx + 1 :]
        idx2 = rest.find(":")
        if idx2 >= 0 and rest[:idx2].isdigit():
            content = rest[idx2 + 1 :]
        else:
            content = rest

        path = Path(path_str)
        if path not in candidate_set:
            continue

        stripped = content.strip()
        m_from = _IMPORT_FROM_RE.match(stripped)
        m_simple = _IMPORT_SIMPLE_RE.match(stripped)
        if not m_from and not m_simple:
            continue

        if m_simple:
            for name in m_simple.group(1).split(","):
                name = name.split(" as ")[0].strip()
                if name:
                    _add_import_with_prefixes(file_to_imports[path], name)
        elif m_from:
            dots, module = m_from.group(1), m_from.group(2)
            if dots:
                resolved = _resolve_relative_rg(path, repo_root, len(dots), module or "")
                if resolved:
                    _add_import_with_prefixes(file_to_imports[path], resolved)
            elif module:
                _add_import_with_prefixes(file_to_imports[path], module)

    return file_to_imports


_REEXPORT_MAX_DEPTH = 3


def _resolve_import_target_dir(node: ast.ImportFrom, pkg_dir: Path, repo_root: Path | None) -> Path | None:
    source_module = node.module or ""
    if node.level and node.level > 0:
        target_dir = pkg_dir
        for _ in range(node.level - 1):
            target_dir = target_dir.parent
        if source_module:
            target_dir = target_dir / Path(*source_module.split("."))
        return target_dir
    if source_module and repo_root:
        return repo_root / Path(*source_module.split("."))
    return None


def _find_module_file(target_dir: Path) -> Path | None:
    source_as_file = target_dir.with_suffix(".py")
    if source_as_file.is_file():
        return source_as_file
    source_as_pkg = target_dir / _INIT_PY
    if source_as_pkg.is_file():
        return source_as_pkg
    for ext in (".pyi", ".pyw"):
        candidate = target_dir.with_suffix(ext)
        if candidate.is_file():
            return candidate
    return None


def _resolve_init_reexports(
    init_path: Path,
    repo_root: Path | None,
    file_cache: dict[Path, str] | None = None,
    _depth: int = 0,
) -> dict[str, Path]:
    if _depth >= _REEXPORT_MAX_DEPTH:
        return {}

    content = file_cache.get(init_path) if file_cache else None
    if content is None:
        try:
            content = init_path.read_text(errors="replace")
        except OSError:
            return {}

    import warnings

    try:
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", SyntaxWarning)
            tree = ast.parse(content)
    except SyntaxError:
        return {}

    result: dict[str, Path] = {}
    pkg_dir = init_path.parent

    for node in ast.walk(tree):
        if not isinstance(node, ast.ImportFrom) or not node.names:
            continue

        target_dir = _resolve_import_target_dir(node, pkg_dir, repo_root)
        if target_dir is None:
            continue

        resolved_path = _find_module_file(target_dir)
        if resolved_path is None:
            continue

        for alias in node.names:
            _name = alias.asname or alias.name
            if _name == "*":
                result[f"*:{resolved_path}"] = resolved_path
                continue
            result[_name] = resolved_path

            if resolved_path.name == _INIT_PY:
                nested = _resolve_init_reexports(resolved_path, repo_root, file_cache, _depth + 1)
                if _name in nested:
                    result[_name] = nested[_name]

    return result


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

        return list(discovered)

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

            for imp in f_imports:
                for target in module_to_files.get(imp, []):
                    if target not in changed_set and target not in discovered:
                        discovered.add(target)
                        next_frontier.add(target)

            f_module = file_to_module.get(f) or path_to_module(f, repo_root)
            if f_module:
                for candidate, cand_imports in file_to_imports.items():
                    if candidate not in changed_set and candidate not in discovered and f_module in cand_imports:
                        discovered.add(candidate)
                        next_frontier.add(candidate)

        return next_frontier

    def _build_import_index(
        self,
        all_candidate_files: list[Path],
        repo_root: Path | None,
        file_cache: dict[Path, str] | None = None,
    ) -> tuple[dict[Path, str], dict[str, list[Path]], dict[Path, set[str]]]:
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
                    prefix = ".".join(parts[:i])
                    module_to_files[prefix].append(f)

        self._enrich_with_reexports(
            all_candidate_files,
            repo_root,
            file_cache,
            file_to_module,
            module_to_files,
        )

        if _RG_BIN and repo_root:
            candidate_set = {f for f in all_candidate_files if _is_python_file(f)}
            try:
                file_to_imports = _build_import_index_rg(repo_root, candidate_set)
                return file_to_module, dict(module_to_files), dict(file_to_imports)
            except (subprocess.TimeoutExpired, OSError):
                logger.debug("ripgrep import index failed, falling back to ast")

        file_to_imports_ast: dict[Path, set[str]] = {}
        for f in all_candidate_files:
            if not _is_python_file(f):
                continue
            content = file_cache.get(f) if file_cache else None
            if content is None:
                try:
                    content = f.read_text(encoding="utf-8")
                except (OSError, UnicodeDecodeError):
                    continue
            file_to_imports_ast[f] = _extract_imports_from_content(content, f, repo_root)

        return file_to_module, dict(module_to_files), file_to_imports_ast

    @staticmethod
    def _enrich_with_reexports(
        all_candidate_files: list[Path],
        repo_root: Path | None,
        file_cache: dict[Path, str] | None,
        file_to_module: dict[Path, str],
        module_to_files: dict[str, list[Path]],
    ) -> None:
        init_files = [f for f in all_candidate_files if f.name == _INIT_PY]
        for init_f in init_files:
            pkg_module = path_to_module(init_f, repo_root)
            if not pkg_module:
                continue
            reexports = _resolve_init_reexports(init_f, repo_root, file_cache)
            existing = set(module_to_files.get(pkg_module, []))
            for _name, source_path in reexports.items():
                if source_path not in existing:
                    module_to_files[pkg_module].append(source_path)
                    existing.add(source_path)
                    if source_path not in file_to_module:
                        src_module = path_to_module(source_path, repo_root)
                        if src_module:
                            file_to_module[source_path] = src_module

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
