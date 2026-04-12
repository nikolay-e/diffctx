from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_YAML_EXTS = {".yaml", ".yml"}
_JSON_EXTS = {".json"}
_OPENAPI_EXTS = _YAML_EXTS | _JSON_EXTS

_OPENAPI_MARKER_RE = re.compile(r'(?:openapi|swagger)\s*[:=]\s*["\']?\d', re.IGNORECASE)

_INTERNAL_REF_RE = re.compile(r"""\$ref\s*:\s*['"]?#/components/(\w{1,100})/([A-Za-z_][\w-]{0,200})""")
_EXTERNAL_REF_RE = re.compile(r"""\$ref\s*:\s*['"]([^'"#\s]{1,300})(?:#[^'"]*)?['"]""")
_JSON_INTERNAL_REF_RE = re.compile(r'"\$ref"\s*:\s*"#/components/(\w{1,100})/([A-Za-z_][\w-]{0,200})"')
_JSON_EXTERNAL_REF_RE = re.compile(r'"\$ref"\s*:\s*"([^"#\s]{1,300})(?:#[^"]*)?"')

_YAML_SCHEMA_DEF_RE = re.compile(r"^\s{2,8}([A-Z][\w-]{0,100})\s*:", re.MULTILINE)
_JSON_SCHEMA_DEF_RE = re.compile(r'"([A-Z][\w-]{0,100})"\s*:\s*\{', re.MULTILINE)

_COMPONENTS_SECTION_RE = re.compile(r"^\s{0,4}components\s*:", re.MULTILINE)
_SCHEMAS_SECTION_RE = re.compile(r"^\s{2,6}schemas\s*:", re.MULTILINE)


_openapi_file_cache: dict[Path, bool] = {}


def _is_openapi_file(path: Path) -> bool:
    resolved = path.resolve()
    cached = _openapi_file_cache.get(resolved)
    if cached is not None:
        return cached

    if path.suffix.lower() not in _OPENAPI_EXTS:
        _openapi_file_cache[resolved] = False
        return False
    name_lower = path.name.lower()
    openapi_name_hints = ("openapi", "swagger", "api-spec", "api_spec", "apispec")
    if any(hint in name_lower for hint in openapi_name_hints):
        _openapi_file_cache[resolved] = True
        return True
    try:
        with open(path, encoding="utf-8") as fh:
            head = fh.read(2048)
        result = bool(_OPENAPI_MARKER_RE.search(head))
    except (OSError, UnicodeDecodeError):
        result = False
    _openapi_file_cache[resolved] = result
    return result


def _is_openapi_content(content: str) -> bool:
    return bool(_OPENAPI_MARKER_RE.search(content[:2000]))


def _extract_internal_refs(content: str) -> set[str]:
    refs: set[str] = set()
    for m in _INTERNAL_REF_RE.finditer(content):
        refs.add(m.group(2))
    for m in _JSON_INTERNAL_REF_RE.finditer(content):
        refs.add(m.group(2))
    return refs


def _extract_external_refs(content: str) -> set[str]:
    refs: set[str] = set()
    for m in _EXTERNAL_REF_RE.finditer(content):
        refs.add(m.group(1))
    for m in _JSON_EXTERNAL_REF_RE.finditer(content):
        refs.add(m.group(1))
    return refs


def _extract_schema_definitions(content: str) -> set[str]:
    defs: set[str] = set()

    if _COMPONENTS_SECTION_RE.search(content) and _SCHEMAS_SECTION_RE.search(content):
        schemas_match = _SCHEMAS_SECTION_RE.search(content)
        if schemas_match:
            after_schemas = content[schemas_match.end() :]
            for m in _YAML_SCHEMA_DEF_RE.finditer(after_schemas[:5000]):
                defs.add(m.group(1))

    for m in _JSON_SCHEMA_DEF_RE.finditer(content):
        name = m.group(1)
        if name[0].isupper():
            defs.add(name)

    return defs


def _collect_openapi_refs(openapi_files: list[Path]) -> set[str]:
    refs: set[str] = set()
    for f in openapi_files:
        try:
            content = f.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        refs.update(_extract_external_refs(content))
    return refs


class OpenapiEdgeBuilder(EdgeBuilder):
    weight = 0.55
    internal_ref_weight = EDGE_WEIGHTS["openapi_internal_ref"].forward
    external_ref_weight = EDGE_WEIGHTS["openapi_external_ref"].forward
    schema_ref_weight = EDGE_WEIGHTS["openapi_schema_ref"].forward
    reverse_weight_factor = EDGE_WEIGHTS["openapi_internal_ref"].reverse_factor

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        openapi_changed = [f for f in changed_files if _is_openapi_file(f)]
        if not openapi_changed:
            return []

        refs = _collect_openapi_refs(openapi_changed)
        return discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        openapi_frags = [f for f in fragments if _is_openapi_content(f.content)]
        if not openapi_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)
        schema_defs = self._build_schema_index(openapi_frags)

        for oaf in openapi_frags:
            self._add_internal_ref_edges(oaf, schema_defs, edges)
            self._add_external_ref_edges(oaf, idx, edges)

        return edges

    def _build_schema_index(self, openapi_frags: list[Fragment]) -> dict[str, list[FragmentId]]:
        defs: dict[str, list[FragmentId]] = defaultdict(list)
        for f in openapi_frags:
            for name in _extract_schema_definitions(f.content):
                defs[name.lower()].append(f.id)
        return defs

    def _add_internal_ref_edges(self, oaf: Fragment, schema_defs: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        own_defs = {n.lower() for n in _extract_schema_definitions(oaf.content)}
        has_schemas = bool(own_defs)
        for ref in _extract_internal_refs(oaf.content):
            ref_lower = ref.lower()
            if ref_lower in own_defs:
                continue
            weight = self.schema_ref_weight if has_schemas else self.internal_ref_weight
            for fid in schema_defs.get(ref_lower, []):
                if fid != oaf.id:
                    self.add_edge(edges, oaf.id, fid, weight)

    def _add_external_ref_edges(self, oaf: Fragment, idx: FragmentIndex, edges: EdgeDict) -> None:
        for ext_ref in _extract_external_refs(oaf.content):
            self.link_by_path_match(oaf.id, ext_ref, idx, edges, self.external_ref_weight)
