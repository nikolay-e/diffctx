from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import LANG_WEIGHTS
from ...javascript_semantics import JsFragmentInfo, analyze_javascript_fragment, extract_import_sources
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, add_semantic_edges

_JS_EXTS = {".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".mts", ".cts"}
_TS_EXTS = {".ts", ".tsx", ".mts", ".cts"}

_EXPORT_DECL_RE = re.compile(
    r"export\s+(?:const|let|var|function\*?|class|async\s+function|interface|type|enum|abstract\s+class)\s+(\w+)",
    re.MULTILINE,
)
_EXPORT_DEFAULT_NAME_RE = re.compile(
    r"export\s+default\s+(?:(?:class|function\*?|async\s+function)\s+)?(\w+)",
    re.MULTILINE,
)
_EXPORT_LIST_RE = re.compile(r"export\s*\{([^}]+)\}", re.MULTILINE)
_NAMED_IMPORT_NAMES_RE = re.compile(
    r"import\s*(?:type\s*)?\{([^}]+)\}\s*from\s*['\"]",
    re.MULTILINE,
)


_JS_KEYWORDS = frozenset(
    {
        "new",
        "class",
        "function",
        "async",
        "await",
        "return",
        "throw",
        "delete",
        "typeof",
        "instanceof",
        "void",
        "yield",
        "super",
        "this",
        "null",
        "undefined",
        "true",
        "false",
    }
)


def _add_name_if_valid(name: str, target: set[str]) -> None:
    if name and len(name) >= 2:
        target.add(name.lower())


def _extract_exports_from_content(content: str, exported: set[str]) -> None:
    for m in _EXPORT_DECL_RE.finditer(content):
        _add_name_if_valid(m.group(1), exported)
    for m in _EXPORT_DEFAULT_NAME_RE.finditer(content):
        captured = m.group(1)
        if captured in _JS_KEYWORDS:
            continue
        _add_name_if_valid(captured, exported)
    for m in _EXPORT_LIST_RE.finditer(content):
        for part in m.group(1).split(","):
            part = part.strip().split(" as ")[0].strip()
            _add_name_if_valid(part, exported)


_JS_WEIGHTS = LANG_WEIGHTS["javascript"]
_TS_WEIGHTS = LANG_WEIGHTS["typescript"]


def _is_js_file(path: Path) -> bool:
    return path.suffix.lower() in _JS_EXTS


def _normalize_import(imp: str, source_path: Path) -> set[str]:
    names: set[str] = set()

    if imp.startswith("."):
        base_dir = source_path.parent
        parts = imp.split("/")
        resolved_parts: list[str] = []

        for part in parts:
            if part == ".":
                continue
            elif part == "..":
                base_dir = base_dir.parent
            else:
                resolved_parts.append(part)

        if resolved_parts:
            base_parts = list(base_dir.parts)
            full_resolved = base_parts + resolved_parts
            names.add("/".join(full_resolved))
            names.add("/".join(resolved_parts))
            names.add(resolved_parts[-1])
    else:
        names.add(imp)
        parts = imp.split("/")
        if len(parts) > 1:
            names.add(parts[-1])

    return names


def _extract_imports_from_content(content: str, source_path: Path) -> set[str]:
    raw_sources = extract_import_sources(content)
    normalized: set[str] = set()
    for source in raw_sources:
        normalized.update(_normalize_import(source, source_path))
    return normalized


def _resolve_relative_import(
    source_file: Path,
    import_source: str,
    candidate_set: set[Path],
) -> Path | None:
    base_dir = source_file.parent
    parts = import_source.split("/")
    resolved = base_dir
    for part in parts:
        if part == ".":
            continue
        elif part == "..":
            resolved = resolved.parent
        else:
            resolved = resolved / part

    for ext in (".ts", ".tsx", ".js", ".jsx", ".mts", ".cts", ".mjs", ".cjs"):
        candidate = resolved.parent / (resolved.name + ext)
        if candidate in candidate_set:
            return candidate

    for index_name in ("index.ts", "index.tsx", "index.js", "index.jsx"):
        candidate = resolved / index_name
        if candidate in candidate_set:
            return candidate

    if resolved in candidate_set:
        return resolved

    return None


class JavaScriptEdgeBuilder(EdgeBuilder):
    weight = 0.70
    reverse_weight_factor = 0.5

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        js_changed = [f for f in changed_files if _is_js_file(f)]
        if not js_changed:
            return []

        changed_set = set(changed_files)
        discovered: set[Path] = set()

        changed_names = self._collect_changed_names(js_changed, repo_root)
        if changed_names:
            discovered.update(self._find_importing_files(all_candidate_files, changed_set, changed_names))

        exported_names = self._collect_exported_names(js_changed)
        if exported_names:
            discovered.update(self._find_files_importing_names(exported_names, all_candidate_files, changed_set))

        discovered.update(self._discover_forward_imports(js_changed, all_candidate_files, changed_set))

        return list(discovered)

    def _collect_changed_names(self, js_changed: list[Path], repo_root: Path | None) -> set[str]:
        changed_names: set[str] = set()
        for f in js_changed:
            stem = f.stem.lower()
            changed_names.add(stem)
            if stem == "index":
                changed_names.add(f.parent.name.lower())

            if repo_root:
                self._add_relative_path_variants(f, repo_root, changed_names)

        return changed_names

    def _add_relative_path_variants(self, f: Path, repo_root: Path, changed_names: set[str]) -> None:
        try:
            rel = f.relative_to(repo_root)
            rel_str = str(rel.with_suffix("")).replace("\\", "/")
            changed_names.add(rel_str)
            parts = rel_str.split("/")
            for i in range(len(parts)):
                changed_names.add("/".join(parts[i:]))
        except ValueError:
            pass

    def _find_importing_files(
        self,
        all_candidate_files: list[Path],
        changed_set: set[Path],
        changed_names: set[str],
    ) -> list[Path]:
        discovered: list[Path] = []

        for candidate in all_candidate_files:
            if candidate in changed_set or not _is_js_file(candidate):
                continue

            if self._imports_changed_name(candidate, changed_names):
                discovered.append(candidate)

        return discovered

    def _imports_changed_name(self, candidate: Path, changed_names: set[str]) -> bool:
        try:
            content = candidate.read_text(encoding="utf-8")
            imports = _extract_imports_from_content(content, candidate)

            for imp in imports:
                imp_lower = imp.lower()
                if any((name in imp_lower or imp_lower.endswith(name)) for name in changed_names if len(name) >= 3):
                    return True
        except (OSError, UnicodeDecodeError):
            pass
        return False

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        js_frags = [f for f in fragments if f.path.suffix.lower() in _JS_EXTS]
        if not js_frags:
            return {}

        info_cache: dict[FragmentId, JsFragmentInfo] = {}
        for f in js_frags:
            info_cache[f.id] = analyze_javascript_fragment(f.content)

        name_to_defs: dict[str, list[FragmentId]] = defaultdict(list)
        frag_defines: dict[FragmentId, frozenset[str]] = {}

        for f in js_frags:
            info = info_cache[f.id]
            frag_defines[f.id] = info.defines
            for name in info.defines:
                name_to_defs[name].append(f.id)

        edges: EdgeDict = {}

        for f in js_frags:
            info = info_cache[f.id]
            self_defs = set(frag_defines.get(f.id, frozenset()))
            w = _TS_WEIGHTS if f.path.suffix.lower() in _TS_EXTS else _JS_WEIGHTS

            add_semantic_edges(
                edges,
                f.id,
                info,
                name_to_defs,
                w.call,
                w.symbol_ref,
                w.type_ref,
                self.reverse_weight_factor,
                self_defs,
            )

        self._add_import_edges(js_frags, info_cache, edges)

        return edges

    _IMPORT_WEIGHT = 0.55

    def _add_import_edges(
        self,
        js_frags: list[Fragment],
        info_cache: dict[FragmentId, JsFragmentInfo],
        edges: EdgeDict,
    ) -> None:
        file_to_frags: dict[Path, list[FragmentId]] = defaultdict(list)
        for f in js_frags:
            file_to_frags[f.path].append(f.id)

        fragment_paths = set(file_to_frags.keys())
        file_imports = self._collect_relative_imports(js_frags, info_cache)

        for src_path, import_sources in file_imports.items():
            for import_source in import_sources:
                resolved = _resolve_relative_import(src_path, import_source, fragment_paths)
                if resolved is None or resolved == src_path:
                    continue
                target_ids = file_to_frags.get(resolved, [])
                if target_ids:
                    self._link_import_pairs(file_to_frags[src_path], target_ids, edges)

    @staticmethod
    def _collect_relative_imports(js_frags: list[Fragment], info_cache: dict[FragmentId, JsFragmentInfo]) -> dict[Path, set[str]]:
        file_imports: dict[Path, set[str]] = defaultdict(set)
        for f in js_frags:
            for import_source in info_cache[f.id].imports:
                if import_source.startswith("."):
                    file_imports[f.path].add(import_source)
        return file_imports

    def _link_import_pairs(
        self,
        src_ids: list[FragmentId],
        target_ids: list[FragmentId],
        edges: EdgeDict,
    ) -> None:
        w = self._IMPORT_WEIGHT
        rev_w = w * self.reverse_weight_factor
        for src_id in src_ids:
            for target_id in target_ids:
                if target_id != src_id:
                    edges[(src_id, target_id)] = max(edges.get((src_id, target_id), 0.0), w)
                    edges[(target_id, src_id)] = max(edges.get((target_id, src_id), 0.0), rev_w)

    def _discover_forward_imports(
        self,
        js_changed: list[Path],
        all_candidate_files: list[Path],
        changed_set: set[Path],
    ) -> list[Path]:
        candidate_set = set(all_candidate_files)
        discovered: list[Path] = []
        for f in js_changed:
            try:
                content = f.read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                continue
            sources = extract_import_sources(content)
            for source in sources:
                if not source.startswith("."):
                    continue
                resolved = _resolve_relative_import(f, source, candidate_set)
                if resolved and resolved not in changed_set and resolved not in discovered:
                    discovered.append(resolved)
        return discovered

    def _collect_exported_names(self, js_changed: list[Path]) -> set[str]:
        exported: set[str] = set()
        for f in js_changed:
            try:
                content = f.read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                continue
            _extract_exports_from_content(content, exported)
        return exported

    def _find_files_importing_names(
        self,
        exported_names: set[str],
        all_candidate_files: list[Path],
        changed_set: set[Path],
    ) -> list[Path]:
        discovered: list[Path] = []
        for candidate in all_candidate_files:
            if candidate in changed_set or not _is_js_file(candidate):
                continue
            try:
                content = candidate.read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                continue
            for m in _NAMED_IMPORT_NAMES_RE.finditer(content):
                names = {n.strip().split(" as ")[0].strip().lower() for n in m.group(1).split(",") if n.strip()}
                if names & exported_names:
                    discovered.append(candidate)
                    break
        return discovered
