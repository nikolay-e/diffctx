from __future__ import annotations

import re
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, discover_files_by_refs

_PRISMA_EXTS = {".prisma"}
_CLIENT_EXTS = {".ts", ".tsx", ".js", ".jsx", ".mjs"}

_MODEL_DEF_RE = re.compile(r"^\s*model\s+(\w{1,200})\s*\{", re.MULTILINE)
_ENUM_DEF_RE = re.compile(r"^\s*enum\s+(\w{1,200})\s*\{", re.MULTILINE)
_TYPE_REF_RE = re.compile(r"^\s+\w+\s+(\w+)[\s?@\[]", re.MULTILINE)
_PRISMA_CLIENT_IMPORT_RE = re.compile(
    r"""(?:from\s+|require\s*\(\s*)['"]@prisma/client['"]""",
    re.MULTILINE,
)
_PRISMA_MODEL_USAGE_RE = re.compile(r"prisma\.(\w{1,200})\.", re.MULTILINE)
_MIGRATION_SQL_RE = re.compile(
    r"""(?:CREATE\s+TABLE|ALTER\s+TABLE|CREATE\s+INDEX)\s+["']?(\w{1,200})["']?""",
    re.MULTILINE | re.IGNORECASE,
)


def _is_prisma_file(path: Path) -> bool:
    return path.suffix.lower() in _PRISMA_EXTS


def _is_client_file(path: Path) -> bool:
    return path.suffix.lower() in _CLIENT_EXTS


def _is_migration_file(path: Path) -> bool:
    return path.suffix.lower() == ".sql" and "migration" in str(path).lower()


def _extract_model_names(content: str) -> set[str]:
    models: set[str] = set()
    for m in _MODEL_DEF_RE.finditer(content):
        models.add(m.group(1))
    return models


def _extract_enum_names(content: str) -> set[str]:
    enums: set[str] = set()
    for m in _ENUM_DEF_RE.finditer(content):
        enums.add(m.group(1))
    return enums


def _extract_type_refs(content: str) -> set[str]:
    refs: set[str] = set()
    for m in _TYPE_REF_RE.finditer(content):
        type_name = m.group(1)
        if type_name[0].isupper():
            refs.add(type_name)
    return refs


def _extract_prisma_model_usages(content: str) -> set[str]:
    usages: set[str] = set()
    for m in _PRISMA_MODEL_USAGE_RE.finditer(content):
        usages.add(m.group(1))
    return usages


def _imports_prisma_client(content: str) -> bool:
    return bool(_PRISMA_CLIENT_IMPORT_RE.search(content))


class PrismaEdgeBuilder(EdgeBuilder):
    weight = 0.60
    schema_weight = EDGE_WEIGHTS["prisma_schema"].forward
    client_weight = EDGE_WEIGHTS["prisma_client"].forward
    reverse_weight_factor = EDGE_WEIGHTS["prisma_schema"].reverse_factor

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        prisma_changed = [f for f in changed_files if _is_prisma_file(f)]
        client_changed = [f for f in changed_files if _is_client_file(f)]
        migration_changed = [f for f in changed_files if _is_migration_file(f)]

        if not prisma_changed and not client_changed and not migration_changed:
            return []

        refs: set[str] = set()

        changed_model_names: set[str] = set()
        for f in prisma_changed:
            try:
                content = f.read_text(encoding="utf-8")
                changed_model_names.update(_extract_model_names(content))
                changed_model_names.update(_extract_enum_names(content))
            except (OSError, UnicodeDecodeError):
                continue

        if prisma_changed:
            self._find_client_consumers(all_candidate_files, changed_model_names, refs)
            for f in prisma_changed:
                refs.add("schema.prisma")

        if client_changed:
            self._find_schema_for_client(client_changed, all_candidate_files, refs)

        if migration_changed:
            self._find_schema_for_migration(migration_changed, all_candidate_files, refs)

        return discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

    def _find_client_consumers(
        self,
        all_candidate_files: list[Path],
        model_names: set[str],
        refs: set[str],
    ) -> None:
        model_lower = {m.lower() for m in model_names}
        for candidate in all_candidate_files:
            if not _is_client_file(candidate):
                continue
            try:
                content = candidate.read_text(encoding="utf-8")
                if not _imports_prisma_client(content):
                    continue
                usages = _extract_prisma_model_usages(content)
                if usages & model_lower:
                    refs.add(candidate.name.lower())
            except (OSError, UnicodeDecodeError):
                continue

    def _find_schema_for_client(
        self,
        client_changed: list[Path],
        all_candidate_files: list[Path],
        refs: set[str],
    ) -> None:
        has_prisma_import = False
        for f in client_changed:
            try:
                content = f.read_text(encoding="utf-8")
                if _imports_prisma_client(content):
                    has_prisma_import = True
                    break
            except (OSError, UnicodeDecodeError):
                continue

        if has_prisma_import:
            for candidate in all_candidate_files:
                if _is_prisma_file(candidate):
                    refs.add(candidate.name.lower())

    def _find_schema_for_migration(
        self,
        migration_changed: list[Path],
        all_candidate_files: list[Path],
        refs: set[str],
    ) -> None:
        table_names: set[str] = set()
        for f in migration_changed:
            try:
                content = f.read_text(encoding="utf-8")
                for m in _MIGRATION_SQL_RE.finditer(content):
                    table_names.add(m.group(1))
            except (OSError, UnicodeDecodeError):
                continue

        if table_names:
            for candidate in all_candidate_files:
                if _is_prisma_file(candidate):
                    refs.add(candidate.name.lower())

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        prisma_frags = [f for f in fragments if _is_prisma_file(f.path)]
        client_frags = [f for f in fragments if _is_client_file(f.path)]
        migration_frags = [f for f in fragments if _is_migration_file(f.path)]

        if not prisma_frags and not client_frags and not migration_frags:
            return {}

        edges: EdgeDict = {}

        model_index = self._build_model_index(prisma_frags)

        self._link_prisma_internals(prisma_frags, model_index, edges)
        self._link_client_to_schema(client_frags, model_index, edges)
        self._link_migration_to_schema(migration_frags, prisma_frags, edges)

        return edges

    def _build_model_index(self, prisma_frags: list[Fragment]) -> dict[str, list[FragmentId]]:
        index: dict[str, list[FragmentId]] = {}
        for f in prisma_frags:
            for model in _extract_model_names(f.content):
                index.setdefault(model.lower(), []).append(f.id)
            for enum in _extract_enum_names(f.content):
                index.setdefault(enum.lower(), []).append(f.id)
        return index

    def _link_prisma_internals(
        self,
        prisma_frags: list[Fragment],
        model_index: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for pf in prisma_frags:
            type_refs = _extract_type_refs(pf.content)
            for ref in type_refs:
                for fid in model_index.get(ref.lower(), []):
                    if fid != pf.id:
                        self.add_edge(edges, pf.id, fid, self.schema_weight)

    def _link_client_to_schema(
        self,
        client_frags: list[Fragment],
        model_index: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for cf in client_frags:
            if not _imports_prisma_client(cf.content):
                continue
            usages = _extract_prisma_model_usages(cf.content)
            for usage in usages:
                for fid in model_index.get(usage.lower(), []):
                    if fid != cf.id:
                        self.add_edge(edges, cf.id, fid, self.client_weight)

    def _link_migration_to_schema(
        self,
        migration_frags: list[Fragment],
        prisma_frags: list[Fragment],
        edges: EdgeDict,
    ) -> None:
        for mf in migration_frags:
            for pf in prisma_frags:
                self.add_edge(edges, mf.id, pf.id, self.schema_weight)
