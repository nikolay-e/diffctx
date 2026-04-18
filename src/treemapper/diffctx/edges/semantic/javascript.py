from __future__ import annotations

import json
import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import LANG_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, add_semantic_edges
from .javascript_semantics import JsFragmentInfo, analyze_javascript_fragment, extract_import_sources

_JS_EXTS = {".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".mts", ".cts"}
_TS_EXTS = {".ts", ".tsx", ".mts", ".cts"}
_JSON_EXT = ".json"

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


_JSONC_LINE_COMMENT_RE = re.compile(r"//.*$", re.MULTILINE)
_JSONC_BLOCK_COMMENT_RE = re.compile(r"/\*.*?\*/", re.DOTALL)
_TRAILING_COMMA_RE = re.compile(r",\s*([}\]])")

_REEXPORT_SOURCE_RE = re.compile(
    r"export\s*(?:\*|\{[^}]*\})\s*from\s*['\"]([^'\"]+)['\"]",
    re.MULTILINE,
)

_MAX_EXTENDS_DEPTH = 5
_REEXPORT_MAX_DEPTH = 2
_DISCOVERY_MAX_DEPTH = 2


def _strip_jsonc_comments(text: str) -> str:
    text = _JSONC_BLOCK_COMMENT_RE.sub("", text)
    text = _JSONC_LINE_COMMENT_RE.sub("", text)
    text = _TRAILING_COMMA_RE.sub(r"\1", text)
    return text


def _resolve_absolute_import(
    resolved_path: Path,
    candidate_set: set[Path],
) -> Path | None:
    for ext in (".ts", ".tsx", ".js", ".jsx", ".mts", ".cts", ".mjs", ".cjs"):
        candidate = resolved_path.parent / (resolved_path.name + ext)
        if candidate in candidate_set:
            return candidate

    for index_name in ("index.ts", "index.tsx", "index.js", "index.jsx"):
        candidate = resolved_path / index_name
        if candidate in candidate_set:
            return candidate

    if resolved_path in candidate_set:
        return resolved_path

    return None


class _TsconfigResolver:
    def __init__(self, repo_root: Path):
        self._repo_root = repo_root
        self._config_cache: dict[Path, dict[str, object] | None] = {}

    def resolve(
        self,
        import_source: str,
        source_file: Path,
        candidate_set: set[Path],
    ) -> Path | None:
        config = self._find_config(source_file)
        if not config:
            return None

        paths = config.get("paths")
        if not isinstance(paths, dict) or not paths:
            return None

        base = self._resolve_base_dir(config)
        return self._match_path_patterns(paths, import_source, base, candidate_set)

    def _resolve_base_dir(self, config: dict[str, object]) -> Path:
        base_url = config.get("baseUrl", ".")
        config_dir = config.get("_config_dir", self._repo_root)
        if not isinstance(config_dir, Path):
            config_dir = Path(str(config_dir))
        if not isinstance(base_url, str):
            base_url = "."
        return config_dir / base_url

    @staticmethod
    def _match_path_patterns(
        paths: dict[str, object],
        import_source: str,
        base: Path,
        candidate_set: set[Path],
    ) -> Path | None:
        for pattern, targets in paths.items():
            if not isinstance(targets, list):
                continue
            prefix = pattern.replace("*", "")
            if not import_source.startswith(prefix):
                continue
            suffix = import_source[len(prefix) :]
            for target in targets:
                if not isinstance(target, str):
                    continue
                resolved_path = (base / target.replace("*", suffix)).resolve()
                result = _resolve_absolute_import(resolved_path, candidate_set)
                if result is not None:
                    return result
        return None

    def _find_config(self, source_file: Path) -> dict[str, object] | None:
        current = source_file.parent
        while True:
            if current in self._config_cache:
                return self._config_cache[current]

            tsconfig_path = current / "tsconfig.json"
            if tsconfig_path.is_file():
                config = self._load_config(tsconfig_path, 0)
                self._config_cache[current] = config
                return config

            if current == self._repo_root or current == current.parent:
                self._config_cache[current] = None
                return None
            current = current.parent

    def _load_config(self, config_path: Path, depth: int) -> dict[str, object] | None:
        if depth >= _MAX_EXTENDS_DEPTH:
            return None
        try:
            raw = config_path.read_text(encoding="utf-8")
            data = json.loads(_strip_jsonc_comments(raw))
        except (OSError, json.JSONDecodeError, UnicodeDecodeError):
            return None

        if not isinstance(data, dict):
            return None

        extends = data.get("extends")
        parent_opts: dict[str, object] = {}
        if isinstance(extends, str):
            parent_path = self._resolve_extends_path(extends, config_path)
            if parent_path and parent_path.is_file():
                parent_config = self._load_config(parent_path, depth + 1)
                if parent_config:
                    parent_opts = dict(parent_config)

        compiler_opts = data.get("compilerOptions", {})
        if not isinstance(compiler_opts, dict):
            compiler_opts = {}

        parent_compiler = parent_opts.get("compilerOptions", {})
        if not isinstance(parent_compiler, dict):
            parent_compiler = {}

        merged_compiler = {**parent_compiler, **compiler_opts}

        result: dict[str, object] = {}
        if "paths" in merged_compiler:
            result["paths"] = merged_compiler["paths"]
        if "baseUrl" in merged_compiler:
            result["baseUrl"] = merged_compiler["baseUrl"]
        result["_config_dir"] = config_path.parent

        return result

    @staticmethod
    def _resolve_extends_path(extends: str, config_path: Path) -> Path | None:
        if extends.startswith("."):
            parent_path = (config_path.parent / extends).resolve()
            if not parent_path.suffix:
                parent_path = parent_path.with_suffix(_JSON_EXT)
            return parent_path
        node_modules = config_path.parent / "node_modules" / extends
        if node_modules.is_file():
            return node_modules
        if node_modules.with_suffix(_JSON_EXT).is_file():
            return node_modules.with_suffix(_JSON_EXT)
        return None


class JavaScriptEdgeBuilder(EdgeBuilder):
    weight = 0.70
    reverse_weight_factor = 0.5

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        js_changed = [f for f in changed_files if _is_js_file(f)]
        if not js_changed:
            return []

        changed_set = set(changed_files)
        discovered: set[Path] = set()
        frontier = list(js_changed)

        for _depth in range(_DISCOVERY_MAX_DEPTH):
            newly_found = self._discover_one_hop(
                frontier,
                all_candidate_files,
                changed_set,
                discovered,
                repo_root,
            )
            if not newly_found:
                break
            discovered.update(newly_found)
            frontier = [f for f in newly_found if _is_js_file(f)]

        return sorted(discovered)

    def _discover_one_hop(
        self,
        frontier_files: list[Path],
        all_candidate_files: list[Path],
        changed_set: set[Path],
        already_discovered: set[Path],
        repo_root: Path | None,
    ) -> list[Path]:
        excluded = changed_set | already_discovered
        hop_discovered: set[Path] = set()

        changed_names = self._collect_changed_names(frontier_files, repo_root)
        if changed_names:
            for f in self._find_importing_files(all_candidate_files, excluded, changed_names):
                if f not in excluded:
                    hop_discovered.add(f)

        exported_names = self._collect_exported_names(frontier_files)
        if exported_names:
            for f in self._find_files_importing_names(exported_names, all_candidate_files, excluded):
                if f not in excluded:
                    hop_discovered.add(f)

        for f in self._discover_forward_imports(frontier_files, all_candidate_files, excluded, repo_root):
            if f not in excluded:
                hop_discovered.add(f)

        return list(hop_discovered)

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

    @staticmethod
    def _build_def_index(
        js_frags: list[Fragment], info_cache: dict[FragmentId, JsFragmentInfo]
    ) -> tuple[dict[str, list[FragmentId]], dict[FragmentId, frozenset[str]]]:
        name_to_defs: dict[str, list[FragmentId]] = defaultdict(list)
        frag_defines: dict[FragmentId, frozenset[str]] = {}
        for f in js_frags:
            info = info_cache[f.id]
            frag_defines[f.id] = info.defines
            for name in info.defines:
                name_to_defs[name].append(f.id)
        return name_to_defs, frag_defines

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        js_frags = [f for f in fragments if f.path.suffix.lower() in _JS_EXTS]
        if not js_frags:
            return {}

        info_cache = {f.id: analyze_javascript_fragment(f.content) for f in js_frags}
        name_to_defs, frag_defines = self._build_def_index(js_frags, info_cache)

        edges: EdgeDict = {}
        for f in js_frags:
            info = info_cache[f.id]
            self_defs = set(frag_defines.get(f.id, frozenset()))
            w = _TS_WEIGHTS if f.path.suffix.lower() in _TS_EXTS else _JS_WEIGHTS
            add_semantic_edges(
                edges, f.id, info, name_to_defs, w.call, w.symbol_ref, w.type_ref, self.reverse_weight_factor, self_defs
            )

        tsconfig_resolver = _TsconfigResolver(repo_root) if repo_root else None
        self._add_import_edges(js_frags, info_cache, edges, tsconfig_resolver)
        return edges

    _IMPORT_WEIGHT = 0.55
    _REEXPORT_WEIGHT_FACTOR = 0.8

    def _link_resolved_import(
        self,
        src_path: Path,
        resolved: Path,
        file_to_frags: dict[Path, list[FragmentId]],
        fragment_paths: set[Path],
        edges: EdgeDict,
    ) -> None:
        if resolved == src_path:
            return
        target_ids = file_to_frags.get(resolved, [])
        if target_ids:
            self._link_import_pairs(file_to_frags[src_path], target_ids, edges)
            self._follow_reexports(resolved, file_to_frags[src_path], file_to_frags, fragment_paths, edges)

    def _add_import_edges(
        self,
        js_frags: list[Fragment],
        info_cache: dict[FragmentId, JsFragmentInfo],
        edges: EdgeDict,
        tsconfig_resolver: _TsconfigResolver | None = None,
    ) -> None:
        file_to_frags: dict[Path, list[FragmentId]] = defaultdict(list)
        for f in js_frags:
            file_to_frags[f.path].append(f.id)

        fragment_paths = set(file_to_frags.keys())
        file_imports, alias_resolved = self._collect_imports(js_frags, info_cache, tsconfig_resolver, fragment_paths)

        for src_path, import_sources in file_imports.items():
            for import_source in import_sources:
                resolved = _resolve_relative_import(src_path, import_source, fragment_paths)
                if resolved is not None:
                    self._link_resolved_import(src_path, resolved, file_to_frags, fragment_paths, edges)

        for src_path, resolved_targets in alias_resolved.items():
            for resolved in resolved_targets:
                self._link_resolved_import(src_path, resolved, file_to_frags, fragment_paths, edges)

    @staticmethod
    def _collect_imports(
        js_frags: list[Fragment],
        info_cache: dict[FragmentId, JsFragmentInfo],
        tsconfig_resolver: _TsconfigResolver | None,
        candidate_set: set[Path],
    ) -> tuple[dict[Path, set[str]], dict[Path, set[Path]]]:
        file_imports: dict[Path, set[str]] = defaultdict(set)
        alias_resolved: dict[Path, set[Path]] = defaultdict(set)
        for f in js_frags:
            for import_source in info_cache[f.id].imports:
                if import_source.startswith("."):
                    file_imports[f.path].add(import_source)
                elif tsconfig_resolver:
                    resolved = tsconfig_resolver.resolve(import_source, f.path, candidate_set)
                    if resolved:
                        alias_resolved[f.path].add(resolved)
        return file_imports, alias_resolved

    def _follow_reexports(
        self,
        target_file: Path,
        src_ids: list[FragmentId],
        file_to_frags: dict[Path, list[FragmentId]],
        fragment_paths: set[Path],
        edges: EdgeDict,
        depth: int = 0,
        visited: set[Path] | None = None,
    ) -> None:
        if visited is None:
            visited = set()
        if target_file in visited:
            return
        visited.add(target_file)
        try:
            content = target_file.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            return
        reexport_sources: set[str] = set()
        for m in _REEXPORT_SOURCE_RE.finditer(content):
            reexport_sources.add(m.group(1))
        if not reexport_sources:
            return
        for source in reexport_sources:
            if not source.startswith("."):
                continue
            resolved = _resolve_relative_import(target_file, source, fragment_paths)
            if resolved is None:
                continue
            reexport_target_ids = file_to_frags.get(resolved, [])
            if reexport_target_ids:
                self._link_reexport_pairs(src_ids, reexport_target_ids, edges)
                if depth < _REEXPORT_MAX_DEPTH:
                    self._follow_reexports(
                        resolved,
                        src_ids,
                        file_to_frags,
                        fragment_paths,
                        edges,
                        depth + 1,
                        visited,
                    )

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

    def _link_reexport_pairs(
        self,
        src_ids: list[FragmentId],
        target_ids: list[FragmentId],
        edges: EdgeDict,
    ) -> None:
        w = self._IMPORT_WEIGHT * self._REEXPORT_WEIGHT_FACTOR
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
        repo_root: Path | None = None,
    ) -> list[Path]:
        candidate_set = set(all_candidate_files)
        tsconfig_resolver = _TsconfigResolver(repo_root) if repo_root else None
        discovered: list[Path] = []
        for f in js_changed:
            try:
                content = f.read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                continue
            sources = extract_import_sources(content)
            for source in sources:
                if source.startswith("."):
                    resolved = _resolve_relative_import(f, source, candidate_set)
                elif tsconfig_resolver:
                    resolved = tsconfig_resolver.resolve(source, f, candidate_set)
                else:
                    continue
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
