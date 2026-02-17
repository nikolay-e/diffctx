from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...javascript_semantics import JsFragmentInfo, analyze_javascript_fragment
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, add_semantic_edges

_JS_EXTS = {".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".mts", ".cts"}

_JS_CALL_WEIGHT = 0.70
_JS_SYMBOL_REF_WEIGHT = 0.75
_JS_TYPE_REF_WEIGHT = 0.65

_JS_IMPORT_STATIC_RE = re.compile(r"""import\s{1,10}[^'"]{0,500}['"]([^'"]{1,500})['"]""")
_JS_REQUIRE_RE = re.compile(r"""require\s{0,10}\(\s{0,10}['"]([^'"]{1,500})['"]\s{0,10}\)""")
_JS_EXPORT_FROM_RE = re.compile(r"""export\s{1,10}[^'"]{0,500}\s{1,10}from\s{1,10}['"]([^'"]{1,500})['"]""")


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
            names.add("/".join(resolved_parts))
            names.add(resolved_parts[-1])
    else:
        names.add(imp)
        parts = imp.split("/")
        if len(parts) > 1:
            names.add(parts[-1])

    return names


def _extract_imports_from_content(content: str, source_path: Path) -> set[str]:
    imports: set[str] = set()

    for match in _JS_IMPORT_STATIC_RE.finditer(content):
        if match.group(1):
            imports.update(_normalize_import(match.group(1), source_path))

    for match in _JS_REQUIRE_RE.finditer(content):
        if match.group(1):
            imports.update(_normalize_import(match.group(1), source_path))

    for match in _JS_EXPORT_FROM_RE.finditer(content):
        if match.group(1):
            imports.update(_normalize_import(match.group(1), source_path))

    return imports


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

        changed_names = self._collect_changed_names(js_changed, repo_root)
        if not changed_names:
            return []

        return self._find_importing_files(all_candidate_files, set(changed_files), changed_names)

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
                if any(name in imp_lower or imp_lower.endswith(name) for name in changed_names):
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

            add_semantic_edges(
                edges,
                f.id,
                info,
                name_to_defs,
                _JS_CALL_WEIGHT,
                _JS_SYMBOL_REF_WEIGHT,
                _JS_TYPE_REF_WEIGHT,
                self.reverse_weight_factor,
                self_defs,
            )

        return edges
