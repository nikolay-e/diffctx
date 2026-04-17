from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict

_SQL_EXTS = {".sql"}

_CREATE_TABLE_RE = re.compile(
    r"^\s{0,20}CREATE\s{1,10}TABLE\s{1,10}(?:IF\s{1,10}NOT\s{1,10}EXISTS\s{1,10})?(\w{1,200})", re.MULTILINE | re.IGNORECASE
)
_ALTER_TABLE_RE = re.compile(
    r"^\s{0,20}ALTER\s{1,10}TABLE\s{1,10}(?:IF\s{1,10}EXISTS\s{1,10})?(\w{1,200})", re.MULTILINE | re.IGNORECASE
)
_DROP_TABLE_RE = re.compile(
    r"^\s{0,20}DROP\s{1,10}TABLE\s{1,10}(?:IF\s{1,10}EXISTS\s{1,10})?(\w{1,200})", re.MULTILINE | re.IGNORECASE
)

_FK_INLINE_RE = re.compile(r"REFERENCES\s{1,10}(\w{1,200})\s*\(", re.IGNORECASE)
_FK_CONSTRAINT_RE = re.compile(
    r"FOREIGN\s{1,10}KEY\s*\([^)]{1,300}\)\s*REFERENCES\s{1,10}(\w{1,200})\s*\(",
    re.IGNORECASE,
)

_CREATE_VIEW_RE = re.compile(
    r"^\s{0,20}CREATE\s{1,10}(?:OR\s{1,10}REPLACE\s{1,10})?(?:MATERIALIZED\s{1,10})?VIEW\s{1,10}(?:IF\s{1,10}NOT\s{1,10}EXISTS\s{1,10})?(\w{1,200})",
    re.MULTILINE | re.IGNORECASE,
)

_CREATE_FUNCTION_RE = re.compile(
    r"^\s{0,20}CREATE\s{1,10}(?:OR\s{1,10}REPLACE\s{1,10})?FUNCTION\s{1,10}(\w{1,200})",
    re.MULTILINE | re.IGNORECASE,
)
_CREATE_PROCEDURE_RE = re.compile(
    r"^\s{0,20}CREATE\s{1,10}(?:OR\s{1,10}REPLACE\s{1,10})?PROCEDURE\s{1,10}(\w{1,200})",
    re.MULTILINE | re.IGNORECASE,
)
_TRIGGER_ON_TABLE_RE = re.compile(
    r"ON\s{1,10}(\w{1,200})\s",
    re.IGNORECASE,
)

_FROM_RE = re.compile(r"\bFROM\s{1,10}(\w{1,200})\b", re.IGNORECASE)
_JOIN_RE = re.compile(r"\bJOIN\s{1,10}(\w{1,200})\b", re.IGNORECASE)
_INTO_RE = re.compile(r"\bINTO\s{1,10}(\w{1,200})\b", re.IGNORECASE)
_UPDATE_RE = re.compile(r"^\s{0,20}UPDATE\s{1,10}(\w{1,200})\b", re.MULTILINE | re.IGNORECASE)
_DELETE_FROM_RE = re.compile(r"^\s{0,20}DELETE\s{1,10}FROM\s{1,10}(\w{1,200})\b", re.MULTILINE | re.IGNORECASE)

_MIGRATION_PREFIX_RE = re.compile(r"^(\d{1,20})[_-]")

_SQL_KEYWORDS = frozenset(
    {
        "select",
        "from",
        "where",
        "insert",
        "update",
        "delete",
        "create",
        "alter",
        "drop",
        "table",
        "index",
        "view",
        "function",
        "procedure",
        "trigger",
        "if",
        "not",
        "exists",
        "null",
        "set",
        "values",
        "into",
        "join",
        "inner",
        "outer",
        "left",
        "right",
        "cross",
        "on",
        "and",
        "or",
        "in",
        "between",
        "like",
        "order",
        "by",
        "group",
        "having",
        "limit",
        "offset",
        "as",
        "case",
        "when",
        "then",
        "else",
        "end",
        "begin",
        "commit",
        "rollback",
        "grant",
        "revoke",
        "cascade",
        "restrict",
        "primary",
        "key",
        "foreign",
        "references",
        "unique",
        "check",
        "default",
        "constraint",
        "with",
        "recursive",
        "union",
        "except",
        "intersect",
        "all",
        "any",
        "some",
        "true",
        "false",
        "is",
        "asc",
        "desc",
        "using",
        "returning",
        "explain",
        "analyze",
        "replace",
        "temporary",
        "temp",
        "materialized",
        "each",
        "row",
        "after",
        "before",
        "instead",
        "of",
        "for",
        "execute",
        "language",
        "plpgsql",
        "sql",
        "declare",
        "return",
        "new",
        "old",
        "raise",
        "notice",
        "exception",
        "perform",
        "found",
        "coalesce",
        "count",
        "sum",
        "avg",
        "min",
        "max",
        "text",
        "integer",
        "varchar",
        "boolean",
        "timestamp",
        "serial",
        "bigint",
        "smallint",
        "numeric",
        "decimal",
        "real",
        "double",
        "precision",
        "date",
        "time",
        "bytea",
        "uuid",
        "jsonb",
        "json",
        "array",
    }
)


def _is_sql_file(path: Path) -> bool:
    return path.suffix.lower() in _SQL_EXTS


def _extract_table_definitions(content: str) -> set[str]:
    tables: set[str] = set()
    tables.update(m.group(1) for m in _CREATE_TABLE_RE.finditer(content))
    return tables


def _extract_view_definitions(content: str) -> set[str]:
    return {m.group(1) for m in _CREATE_VIEW_RE.finditer(content)}


def _extract_function_definitions(content: str) -> set[str]:
    funcs: set[str] = set()
    funcs.update(m.group(1) for m in _CREATE_FUNCTION_RE.finditer(content))
    funcs.update(m.group(1) for m in _CREATE_PROCEDURE_RE.finditer(content))
    return funcs


def _extract_fk_references(content: str) -> set[str]:
    refs: set[str] = set()
    refs.update(m.group(1) for m in _FK_INLINE_RE.finditer(content))
    refs.update(m.group(1) for m in _FK_CONSTRAINT_RE.finditer(content))
    return refs


def _extract_table_references(content: str) -> set[str]:
    refs: set[str] = set()
    refs.update(m.group(1) for m in _FROM_RE.finditer(content))
    refs.update(m.group(1) for m in _JOIN_RE.finditer(content))
    refs.update(m.group(1) for m in _INTO_RE.finditer(content))
    refs.update(m.group(1) for m in _UPDATE_RE.finditer(content))
    refs.update(m.group(1) for m in _DELETE_FROM_RE.finditer(content))
    refs.update(m.group(1) for m in _ALTER_TABLE_RE.finditer(content))
    refs.update(m.group(1) for m in _DROP_TABLE_RE.finditer(content))
    refs.update(m.group(1) for m in _TRIGGER_ON_TABLE_RE.finditer(content))
    return {r for r in refs if r.lower() not in _SQL_KEYWORDS}


def _get_migration_order(path: Path) -> int | None:
    m = _MIGRATION_PREFIX_RE.match(path.name)
    return int(m.group(1)) if m else None


class SqlEdgeBuilder(EdgeBuilder):
    weight = 0.55
    fk_weight = EDGE_WEIGHTS["sql_fk"].forward
    table_ref_weight = EDGE_WEIGHTS["sql_table_ref"].forward
    view_source_weight = EDGE_WEIGHTS["sql_view_source"].forward
    migration_weight = EDGE_WEIGHTS["sql_migration"].forward
    reverse_weight_factor = EDGE_WEIGHTS["sql_fk"].reverse_factor

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        sql_changed = [f for f in changed_files if _is_sql_file(f)]
        if not sql_changed:
            return []

        changed_set = set(changed_files)
        discovered: set[Path] = set()

        changed_tables = self._collect_table_names(sql_changed)
        if changed_tables:
            discovered.update(self._find_files_referencing_tables(all_candidate_files, changed_set, changed_tables))

        discovered.update(self._find_adjacent_migrations(sql_changed, all_candidate_files, changed_set))

        return list(discovered)

    def _collect_table_names(self, sql_files: list[Path]) -> set[str]:
        tables: set[str] = set()
        for f in sql_files:
            try:
                content = f.read_text(encoding="utf-8")
                tables.update(_extract_table_definitions(content))
                tables.update(_extract_view_definitions(content))
                tables.update(_extract_table_references(content))
            except (OSError, UnicodeDecodeError):
                continue
        return tables

    def _find_files_referencing_tables(
        self, all_candidate_files: list[Path], changed_set: set[Path], target_tables: set[str]
    ) -> list[Path]:
        target_lower = {t.lower() for t in target_tables}
        discovered: list[Path] = []
        for candidate in all_candidate_files:
            if candidate in changed_set:
                continue
            try:
                content = candidate.read_text(encoding="utf-8")
                if _is_sql_file(candidate):
                    all_refs = _extract_table_references(content)
                    all_refs.update(_extract_fk_references(content))
                    all_refs.update(_extract_table_definitions(content))
                    if any(r.lower() in target_lower for r in all_refs):
                        discovered.append(candidate)
                else:
                    content_lower = content.lower()
                    if any(t in content_lower for t in target_lower if len(t) >= 3):
                        discovered.append(candidate)
            except (OSError, UnicodeDecodeError):
                continue
        return discovered

    def _find_adjacent_migrations(
        self, sql_changed: list[Path], all_candidate_files: list[Path], changed_set: set[Path]
    ) -> list[Path]:
        changed_orders: dict[Path, set[int]] = defaultdict(set)
        for f in sql_changed:
            order = _get_migration_order(f)
            if order is not None:
                changed_orders[f.parent].add(order)

        if not changed_orders:
            return []

        discovered: list[Path] = []
        for candidate in all_candidate_files:
            if candidate in changed_set or not _is_sql_file(candidate):
                continue
            order = _get_migration_order(candidate)
            if order is None:
                continue
            parent_orders = changed_orders.get(candidate.parent)
            if parent_orders and any(abs(order - co) <= 1 for co in parent_orders):
                discovered.append(candidate)

        return discovered

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        sql_frags = [f for f in fragments if _is_sql_file(f.path)]
        if not sql_frags:
            return {}

        edges: EdgeDict = {}
        table_defs = self._build_table_index(sql_frags)

        for sf in sql_frags:
            self._add_fk_edges(sf, table_defs, edges)
            self._add_table_ref_edges(sf, table_defs, edges)
            self._add_view_source_edges(sf, table_defs, edges)
            self._add_function_table_edges(sf, table_defs, edges)

        self._add_migration_order_edges(sql_frags, edges)

        return edges

    def _build_table_index(self, sql_frags: list[Fragment]) -> dict[str, list[FragmentId]]:
        defs: dict[str, list[FragmentId]] = defaultdict(list)
        for f in sql_frags:
            for table in _extract_table_definitions(f.content):
                defs[table.lower()].append(f.id)
            for view in _extract_view_definitions(f.content):
                defs[view.lower()].append(f.id)
        return defs

    def _add_fk_edges(self, sf: Fragment, table_defs: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        for fk_table in _extract_fk_references(sf.content):
            for fid in table_defs.get(fk_table.lower(), []):
                if fid != sf.id:
                    self.add_edge(edges, sf.id, fid, self.fk_weight)

    def _add_table_ref_edges(self, sf: Fragment, table_defs: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        own_tables = {t.lower() for t in _extract_table_definitions(sf.content)}
        own_tables.update(v.lower() for v in _extract_view_definitions(sf.content))

        for ref in _extract_table_references(sf.content):
            ref_lower = ref.lower()
            if ref_lower in own_tables:
                continue
            for fid in table_defs.get(ref_lower, []):
                if fid != sf.id:
                    self.add_edge(edges, sf.id, fid, self.table_ref_weight)

    def _add_view_source_edges(self, sf: Fragment, table_defs: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        views = _extract_view_definitions(sf.content)
        if not views:
            return
        source_tables = _extract_table_references(sf.content)
        for table_ref in source_tables:
            for fid in table_defs.get(table_ref.lower(), []):
                if fid != sf.id:
                    self.add_edge(edges, sf.id, fid, self.view_source_weight)

    def _add_function_table_edges(self, sf: Fragment, table_defs: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        funcs = _extract_function_definitions(sf.content)
        if not funcs:
            return
        table_refs = _extract_table_references(sf.content)
        for table_ref in table_refs:
            for fid in table_defs.get(table_ref.lower(), []):
                if fid != sf.id:
                    self.add_edge(edges, sf.id, fid, self.table_ref_weight)

    def _add_migration_order_edges(self, sql_frags: list[Fragment], edges: EdgeDict) -> None:
        by_dir: dict[Path, list[tuple[int, Fragment]]] = defaultdict(list)
        for f in sql_frags:
            order = _get_migration_order(f.path)
            if order is not None:
                by_dir[f.path.parent].append((order, f))

        for _dir, ordered_frags in by_dir.items():
            ordered_frags.sort(key=lambda x: x[0])
            for i in range(len(ordered_frags) - 1):
                curr_frag = ordered_frags[i][1]
                next_frag = ordered_frags[i + 1][1]
                self.add_edge(edges, curr_frag.id, next_frag.id, self.migration_weight)
