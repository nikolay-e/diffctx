from __future__ import annotations

import re
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_DBT_SQL_EXTS = {".sql"}
_DBT_YAML_EXTS = {".yml", ".yaml"}

_REF_RE = re.compile(r"""\{\{\s*ref\(\s*['"]([^'"]{1,200})['"]\s*\)\s*\}\}""")
_SOURCE_RE = re.compile(r"""\{\{\s*source\(\s*['"]([^'"]{1,200})['"]\s*,\s*['"]([^'"]{1,200})['"]\s*\)\s*\}\}""")
_MACRO_CALL_RE = re.compile(r"""\{\{\s*([a-zA-Z_]\w{0,200})\s*\(""")
_MACRO_DEF_RE = re.compile(r"""\{%[-\s]*macro\s+([a-zA-Z_]\w{0,200})\s*\(""")
_SCHEMA_MODEL_NAME_RE = re.compile(r"^\s*-\s*name:\s*(\w{1,200})", re.MULTILINE)

_DBT_JINJA_BUILTINS = frozenset(
    {
        "config",
        "set",
        "if",
        "elif",
        "else",
        "endif",
        "for",
        "endfor",
        "block",
        "endblock",
        "extends",
        "include",
        "import",
        "from",
        "macro",
        "endmacro",
        "call",
        "endcall",
        "filter",
        "endfilter",
        "raw",
        "endraw",
        "is",
        "in",
        "not",
        "and",
        "or",
        "true",
        "false",
        "none",
        "loop",
        "super",
        "self",
        "caller",
        "varargs",
        "kwargs",
        "log",
        "return",
        "do",
        "with",
        "endwith",
        "autoescape",
        "this",
        "adapter",
        "run_query",
        "statement",
        "load_result",
        "env_var",
        "var",
        "target",
        "builtins",
        "exceptions",
        "graph",
    }
)


def _is_dbt_sql(path: Path) -> bool:
    return path.suffix.lower() in _DBT_SQL_EXTS


def _is_dbt_yaml(path: Path) -> bool:
    return path.suffix.lower() in _DBT_YAML_EXTS


def _is_dbt_file(path: Path) -> bool:
    return _is_dbt_sql(path) or _is_dbt_yaml(path)


def _extract_refs_from_sql(content: str) -> set[str]:
    refs: set[str] = set()
    for m in _REF_RE.finditer(content):
        refs.add(m.group(1))
    return refs


def _extract_sources_from_sql(content: str) -> set[tuple[str, str]]:
    sources: set[tuple[str, str]] = set()
    for m in _SOURCE_RE.finditer(content):
        sources.add((m.group(1), m.group(2)))
    return sources


def _extract_macro_calls(content: str) -> set[str]:
    calls: set[str] = set()
    for m in _MACRO_CALL_RE.finditer(content):
        name = m.group(1)
        if name not in _DBT_JINJA_BUILTINS:
            calls.add(name)
    return calls


def _extract_macro_defs(content: str) -> set[str]:
    defs: set[str] = set()
    for m in _MACRO_DEF_RE.finditer(content):
        defs.add(m.group(1))
    return defs


def _extract_schema_model_names(content: str) -> set[str]:
    names: set[str] = set()
    for m in _SCHEMA_MODEL_NAME_RE.finditer(content):
        names.add(m.group(1))
    return names


def _process_source_table_line(
    line: str, stripped: str, indent: int, in_sources: bool, in_tables: bool, tables_indent: int, tables: set[str]
) -> tuple[bool, bool, int]:
    if stripped.startswith("sources:"):
        return True, False, tables_indent
    if in_sources and stripped.startswith("tables:"):
        return in_sources, True, indent
    if in_tables:
        if indent <= tables_indent:
            return in_sources, False, tables_indent
        m = _SCHEMA_MODEL_NAME_RE.match(line)
        if m:
            tables.add(m.group(1))
    return in_sources, in_tables, tables_indent


def _extract_source_table_names(content: str) -> set[str]:
    in_sources = False
    in_tables = False
    tables_indent = 0
    tables: set[str] = set()
    for line in content.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        indent = len(line) - len(line.lstrip())
        in_sources, in_tables, tables_indent = _process_source_table_line(
            line, stripped, indent, in_sources, in_tables, tables_indent, tables
        )
    return tables


class DbtEdgeBuilder(EdgeBuilder):
    weight = 0.60
    ref_weight = EDGE_WEIGHTS["dbt_ref"].forward
    source_weight = EDGE_WEIGHTS["dbt_source"].forward
    macro_weight = EDGE_WEIGHTS["dbt_macro"].forward
    reverse_weight_factor = EDGE_WEIGHTS["dbt_ref"].reverse_factor

    def _collect_refs_from_sql(
        self, content: str, refs: set[str], macro_names: set[str], source_tables: set[str], model_names: set[str]
    ) -> None:
        for ref_name in _extract_refs_from_sql(content):
            refs.add(ref_name.lower())
            refs.add(f"{ref_name}.sql")
        for _, table in _extract_sources_from_sql(content):
            source_tables.add(table)
        macro_names.update(_extract_macro_calls(content))
        for macro_def in _extract_macro_defs(content):
            model_names.add(macro_def)

    def _collect_refs_from_yaml(self, content: str, refs: set[str], _source_tables: set[str]) -> None:
        for name in _extract_schema_model_names(content):
            refs.add(name.lower())
            refs.add(f"{name}.sql")

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        dbt_changed = [f for f in changed_files if _is_dbt_file(f)]
        if not dbt_changed:
            return []

        refs: set[str] = set()
        macro_names: set[str] = set()
        source_tables: set[str] = set()
        model_names: set[str] = set()

        for f in dbt_changed:
            try:
                content = f.read_text(encoding="utf-8")
                if _is_dbt_sql(f):
                    self._collect_refs_from_sql(content, refs, macro_names, source_tables, model_names)
                if _is_dbt_yaml(f):
                    self._collect_refs_from_yaml(content, refs, source_tables)
            except (OSError, UnicodeDecodeError):
                continue

        self._discover_macro_files(macro_names, all_candidate_files, refs)
        self._discover_reverse_refs(dbt_changed, all_candidate_files, refs, model_names)
        self._discover_source_consumers(source_tables, all_candidate_files, refs)

        return discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

    def _discover_macro_files(
        self,
        macro_names: set[str],
        all_candidate_files: list[Path],
        refs: set[str],
    ) -> None:
        if not macro_names:
            return
        for candidate in all_candidate_files:
            if not _is_dbt_sql(candidate):
                continue
            try:
                content = candidate.read_text(encoding="utf-8")
                defs = _extract_macro_defs(content)
                if defs & macro_names:
                    refs.add(candidate.name.lower())
            except (OSError, UnicodeDecodeError):
                continue

    def _candidate_references_changed_model(self, content: str, changed_model_names: set[str]) -> bool:
        for ref_name in _extract_refs_from_sql(content):
            if ref_name.lower() in changed_model_names:
                return True
        for macro_call in _extract_macro_calls(content):
            if macro_call.lower() in changed_model_names:
                return True
        return False

    def _discover_reverse_refs(
        self,
        dbt_changed: list[Path],
        all_candidate_files: list[Path],
        refs: set[str],
        model_names: set[str],
    ) -> None:
        changed_model_names: set[str] = set(model_names)
        for f in dbt_changed:
            if _is_dbt_sql(f):
                changed_model_names.add(f.stem.lower())

        if not changed_model_names:
            return

        for candidate in all_candidate_files:
            if not _is_dbt_sql(candidate):
                continue
            try:
                content = candidate.read_text(encoding="utf-8")
                if self._candidate_references_changed_model(content, changed_model_names):
                    refs.add(candidate.name.lower())
            except (OSError, UnicodeDecodeError):
                continue

    def _discover_source_consumers(
        self,
        source_tables: set[str],
        all_candidate_files: list[Path],
        refs: set[str],
    ) -> None:
        if not source_tables:
            return
        for candidate in all_candidate_files:
            if not _is_dbt_sql(candidate):
                continue
            try:
                content = candidate.read_text(encoding="utf-8")
                for _, table in _extract_sources_from_sql(content):
                    if table in source_tables:
                        refs.add(candidate.name.lower())
                        break
            except (OSError, UnicodeDecodeError):
                continue

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        dbt_frags = [f for f in fragments if _is_dbt_file(f.path)]
        if not dbt_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)

        macro_index = self._build_macro_index(dbt_frags)
        model_index = self._build_model_index(dbt_frags)
        schema_model_index = self._build_schema_model_index(dbt_frags)

        for df in dbt_frags:
            self._add_fragment_edges(df, idx, edges, macro_index, model_index, schema_model_index)

        return edges

    def _build_macro_index(self, dbt_frags: list[Fragment]) -> dict[str, list[FragmentId]]:
        index: dict[str, list[FragmentId]] = {}
        for f in dbt_frags:
            if _is_dbt_sql(f.path):
                for macro_name in _extract_macro_defs(f.content):
                    index.setdefault(macro_name.lower(), []).append(f.id)
        return index

    def _build_model_index(self, dbt_frags: list[Fragment]) -> dict[str, list[FragmentId]]:
        index: dict[str, list[FragmentId]] = {}
        for f in dbt_frags:
            if _is_dbt_sql(f.path):
                model_name = f.path.stem.lower()
                index.setdefault(model_name, []).append(f.id)
        return index

    def _build_schema_model_index(self, dbt_frags: list[Fragment]) -> dict[str, list[FragmentId]]:
        index: dict[str, list[FragmentId]] = {}
        for f in dbt_frags:
            if _is_dbt_yaml(f.path):
                for name in _extract_schema_model_names(f.content):
                    index.setdefault(name.lower(), []).append(f.id)
        return index

    def _add_sql_fragment_edges(
        self,
        df: Fragment,
        edges: EdgeDict,
        macro_index: dict[str, list[FragmentId]],
        model_index: dict[str, list[FragmentId]],
        schema_model_index: dict[str, list[FragmentId]],
    ) -> None:
        for ref_name in _extract_refs_from_sql(df.content):
            self._link_model_ref(df.id, ref_name, model_index, edges, self.ref_weight)

        for macro_name in _extract_macro_calls(df.content):
            for fid in macro_index.get(macro_name.lower(), []):
                if fid != df.id:
                    self.add_edge(edges, df.id, fid, self.macro_weight)

        model_name = df.path.stem.lower()
        for fid in schema_model_index.get(model_name, []):
            if fid != df.id:
                self.add_edge(edges, df.id, fid, self.ref_weight)

    def _add_yaml_fragment_edges(
        self,
        df: Fragment,
        idx: FragmentIndex,
        edges: EdgeDict,
        model_index: dict[str, list[FragmentId]],
    ) -> None:
        for name in _extract_schema_model_names(df.content):
            for fid in model_index.get(name.lower(), []):
                if fid != df.id:
                    self.add_edge(edges, df.id, fid, self.ref_weight)

        for table in _extract_source_table_names(df.content):
            self._link_source_table(df.id, table, idx, edges)

    def _add_fragment_edges(
        self,
        df: Fragment,
        idx: FragmentIndex,
        edges: EdgeDict,
        macro_index: dict[str, list[FragmentId]],
        model_index: dict[str, list[FragmentId]],
        schema_model_index: dict[str, list[FragmentId]],
    ) -> None:
        if _is_dbt_sql(df.path):
            self._add_sql_fragment_edges(df, edges, macro_index, model_index, schema_model_index)
        if _is_dbt_yaml(df.path):
            self._add_yaml_fragment_edges(df, idx, edges, model_index)

    def _link_model_ref(
        self,
        src_id: FragmentId,
        ref_name: str,
        model_index: dict[str, list[FragmentId]],
        edges: EdgeDict,
        weight: float,
    ) -> None:
        for fid in model_index.get(ref_name.lower(), []):
            if fid != src_id:
                self.add_edge(edges, src_id, fid, weight)

    def _link_source_table(
        self,
        src_id: FragmentId,
        table: str,
        idx: FragmentIndex,
        edges: EdgeDict,
    ) -> None:
        target_name = f"stg_{table}.sql".lower()
        for name, frag_ids in idx.by_name.items():
            if name == target_name or name == f"{table}.sql":
                for fid in frag_ids:
                    if fid != src_id:
                        self.add_edge(edges, src_id, fid, self.source_weight)
                        return
