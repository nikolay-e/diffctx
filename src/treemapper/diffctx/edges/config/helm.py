from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict

_HELM_VALUES_RE = re.compile(r"\{\{\s*\.Values\.([a-zA-Z0-9_.]+)\s*\}\}")
_HELM_INCLUDE_RE = re.compile(r'\{\{\s*(?:include|template)\s+"([^"]+)"')
_HELM_DEFINE_RE = re.compile(r'\{\{-?\s*define\s+"([^"]+)"')
_HELM_RELEASE_RE = re.compile(r"\{\{\s*\.Release\.(\w+)\s*\}\}")
_HELM_CHART_RE = re.compile(r"\{\{\s*\.Chart\.(\w+)\s*\}\}")
_HELM_FILES_RE = re.compile(r'\{\{\s*\.Files\.(?:Get|Glob)\s+"([^"]+)"')

_YAML_KEY_PATH_RE = re.compile(r"^(\s*)([a-zA-Z_][a-zA-Z0-9_-]*):", re.MULTILINE)


def _is_helm_template(path: Path) -> bool:
    parts = path.parts
    return "templates" in parts and path.suffix.lower() in {".yaml", ".yml", ".tpl"}


def _is_helm_values(path: Path) -> bool:
    name = path.name.lower()
    return name in {"values.yaml", "values.yml"} or name.startswith("values-") or name.startswith("values_")


def _is_chart_yaml(path: Path) -> bool:
    return path.name.lower() in {"chart.yaml", "chart.yml"}


def _extract_yaml_keys(content: str, max_depth: int = 4) -> set[str]:
    keys: set[str] = set()
    path_stack: list[tuple[int, str]] = []

    for match in _YAML_KEY_PATH_RE.finditer(content):
        indent = len(match.group(1))
        key = match.group(2)

        while path_stack and path_stack[-1][0] >= indent:
            path_stack.pop()

        path_stack.append((indent, key))

        if len(path_stack) <= max_depth:
            full_path = ".".join(k for _, k in path_stack)
            keys.add(full_path)
            keys.add(key)

    return keys


def _get_chart_root(path: Path) -> Path | None:
    current = path.parent
    for _ in range(5):
        if (current / "Chart.yaml").exists() or (current / "chart.yaml").exists():
            return current
        if current.parent == current:
            break
        current = current.parent
    return None


def _collect_chart_roots(paths: list[Path]) -> set[Path]:
    roots: set[Path] = set()
    for p in paths:
        root = _get_chart_root(p)
        if root:
            roots.add(root)
    return roots


def _is_in_chart(candidate: Path, chart_roots: set[Path]) -> bool:
    for chart_root in chart_roots:
        try:
            if candidate.is_relative_to(chart_root):
                return True
        except (ValueError, TypeError):
            continue
    return False


class HelmEdgeBuilder(EdgeBuilder):
    weight = 0.70
    reverse_weight_factor = 0.45

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        templates = [f for f in changed_files if _is_helm_template(f)]
        values = [f for f in changed_files if _is_helm_values(f)]

        if not templates and not values:
            return []

        chart_roots = _collect_chart_roots(templates + values)
        if not chart_roots:
            return []

        changed_set = set(changed_files)
        discovered: list[Path] = []

        for candidate in all_candidate_files:
            if candidate in changed_set:
                continue
            if not (_is_helm_template(candidate) or _is_helm_values(candidate) or _is_chart_yaml(candidate)):
                continue
            if _is_in_chart(candidate, chart_roots):
                discovered.append(candidate)

        return discovered

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        templates = [f for f in fragments if _is_helm_template(f.path)]
        values_files = [f for f in fragments if _is_helm_values(f.path)]
        chart_files = [f for f in fragments if _is_chart_yaml(f.path)]

        if not templates and not values_files:
            return {}

        edges: EdgeDict = {}
        values_idx = self._build_values_index(values_files)
        define_defs = self._build_define_index(templates)

        for tmpl in templates:
            self._add_template_edges(tmpl, values_idx, define_defs, chart_files, edges)

        return edges

    def _build_values_index(self, values_files: list[Fragment]) -> dict[Path, dict[str, list[FragmentId]]]:
        idx: dict[Path, dict[str, list[FragmentId]]] = defaultdict(lambda: defaultdict(list))
        for vf in values_files:
            chart_root = _get_chart_root(vf.path) or vf.path.parent
            for key in _extract_yaml_keys(vf.content):
                idx[chart_root][key].append(vf.id)
        return idx

    def _build_define_index(self, templates: list[Fragment]) -> dict[str, list[FragmentId]]:
        defs: dict[str, list[FragmentId]] = defaultdict(list)
        for tmpl in templates:
            for match in _HELM_DEFINE_RE.finditer(tmpl.content):
                defs[match.group(1)].append(tmpl.id)
        return defs

    def _add_template_edges(
        self,
        tmpl: Fragment,
        values_idx: dict[Path, dict[str, list[FragmentId]]],
        define_defs: dict[str, list[FragmentId]],
        chart_files: list[Fragment],
        edges: EdgeDict,
    ) -> None:
        chart_root = _get_chart_root(tmpl.path) or tmpl.path.parent.parent
        values_keys = values_idx.get(chart_root, {})

        self._add_values_ref_edges(tmpl, values_keys, edges)
        self._add_include_edges(tmpl, define_defs, edges)
        self._add_chart_file_edges(tmpl, chart_root, chart_files, edges)

    def _add_values_ref_edges(self, tmpl: Fragment, values_keys: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        for match in _HELM_VALUES_RE.finditer(tmpl.content):
            parts = match.group(1).split(".")
            self._link_longest_match(tmpl.id, parts, values_keys, edges)
            self._link_root_key(tmpl.id, parts[0], values_keys, edges)

    def _link_longest_match(
        self,
        tmpl_id: FragmentId,
        parts: list[str],
        values_keys: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for i in range(len(parts), 0, -1):
            partial = ".".join(parts[:i])
            values_ids = values_keys.get(partial, [])
            if values_ids:
                self.add_edge(edges, tmpl_id, values_ids[0], self.weight)
                return

    def _link_root_key(
        self,
        tmpl_id: FragmentId,
        root_key: str,
        values_keys: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for values_id in values_keys.get(root_key, []):
            self.add_edge(edges, tmpl_id, values_id, self.weight * 0.8)

    def _add_include_edges(self, tmpl: Fragment, define_defs: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        for match in _HELM_INCLUDE_RE.finditer(tmpl.content):
            for def_id in define_defs.get(match.group(1), []):
                if def_id != tmpl.id:
                    self.add_edge(edges, tmpl.id, def_id, self.weight * 0.9)

    def _add_chart_file_edges(self, tmpl: Fragment, chart_root: Path, chart_files: list[Fragment], edges: EdgeDict) -> None:
        for cf in chart_files:
            cf_root = _get_chart_root(cf.path) or cf.path.parent
            if cf_root == chart_root:
                self.add_edge(edges, tmpl.id, cf.id, self.weight * 0.5)
