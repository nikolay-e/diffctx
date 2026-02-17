from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, build_path_to_frags

_TF_EXTENSIONS = {".tf", ".tfvars", ".hcl"}

_TF_VARIABLE_RE = re.compile(r'^variable\s+"([^"]+)"', re.MULTILINE)
_TF_RESOURCE_RE = re.compile(r'^resource\s+"([^"]+)"\s+"([^"]+)"', re.MULTILINE)
_TF_DATA_RE = re.compile(r'^data\s+"([^"]+)"\s+"([^"]+)"', re.MULTILINE)
_TF_OUTPUT_RE = re.compile(r'^output\s+"([^"]+)"', re.MULTILINE)
_TF_MODULE_RE = re.compile(r'^module\s+"([^"]+)"', re.MULTILINE)
_TF_LOCALS_RE = re.compile(r"^locals\s*\{", re.MULTILINE)
_TF_LOCAL_KEY_RE = re.compile(r"^\s+(\w+)\s*=", re.MULTILINE)

_TF_VAR_REF_RE = re.compile(r"var\.(\w+)")
_TF_LOCAL_REF_RE = re.compile(r"local\.(\w+)")
_TF_DATA_REF_RE = re.compile(r"data\.(\w+)\.(\w+)")
_TF_RESOURCE_REF_RE = re.compile(r"(?<![.\w])(\w+)\.(\w+)\.(\w+)")
_TF_MODULE_REF_RE = re.compile(r"module\.(\w+)")

_TF_SOURCE_RE = re.compile(r'^\s*source\s*=\s*"([^"]+)"', re.MULTILINE)


def _is_terraform_file(path: Path) -> bool:
    return path.suffix.lower() in _TF_EXTENSIONS


def _extract_locals(content: str) -> set[str]:
    locals_keys: set[str] = set()
    in_locals = False
    brace_count = 0

    for line in content.splitlines():
        if _TF_LOCALS_RE.match(line):
            in_locals = True
            brace_count = line.count("{") - line.count("}")
            if brace_count <= 0:
                in_locals = False
            continue

        if in_locals:
            brace_count += line.count("{") - line.count("}")
            if brace_count <= 0:
                in_locals = False
                continue

            match = _TF_LOCAL_KEY_RE.match(line)
            if match:
                locals_keys.add(match.group(1))

    return locals_keys


def _collect_tf_dirs_and_sources(tf_files: list[Path]) -> tuple[set[Path], set[str]]:
    tf_dirs: set[Path] = set()
    module_sources: set[str] = set()

    for tf in tf_files:
        tf_dirs.add(tf.parent)
        try:
            content = tf.read_text(encoding="utf-8")
            for match in _TF_SOURCE_RE.finditer(content):
                src = match.group(1)
                if src.startswith("./") or src.startswith("../"):
                    module_sources.add(src)
        except (OSError, UnicodeDecodeError):
            continue

    return tf_dirs, module_sources


def _is_in_module(candidate: Path, module_sources: set[str], tf_dirs: set[Path]) -> bool:
    for src in module_sources:
        for tf_dir in tf_dirs:
            try:
                module_path = (tf_dir / src).resolve()
                if candidate.is_relative_to(module_path):
                    return True
            except (ValueError, OSError):
                continue
    return False


class _TFIndex:
    var_defs: dict[str, list[FragmentId]]
    resource_defs: dict[str, list[FragmentId]]
    data_defs: dict[str, list[FragmentId]]
    local_defs: dict[str, list[FragmentId]]
    module_defs: dict[str, list[FragmentId]]

    def __init__(self) -> None:
        self.var_defs = defaultdict(list)
        self.resource_defs = defaultdict(list)
        self.data_defs = defaultdict(list)
        self.local_defs = defaultdict(list)
        self.module_defs = defaultdict(list)


class TerraformEdgeBuilder(EdgeBuilder):
    weight = 0.60
    reverse_weight_factor = 0.40

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        tf_files = [f for f in changed_files if _is_terraform_file(f)]
        if not tf_files:
            return []

        tf_dirs, module_sources = _collect_tf_dirs_and_sources(tf_files)
        changed_set = set(changed_files)
        discovered: list[Path] = []

        for candidate in all_candidate_files:
            if candidate in changed_set or not _is_terraform_file(candidate):
                continue
            if candidate.parent in tf_dirs or _is_in_module(candidate, module_sources, tf_dirs):
                discovered.append(candidate)

        return discovered

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        tf_frags = [f for f in fragments if _is_terraform_file(f.path)]
        if not tf_frags:
            return {}

        edges: EdgeDict = {}
        idx = self._build_index(tf_frags)

        for f in tf_frags:
            self._add_ref_edges(f, idx, edges)

        self._build_module_source_edges(tf_frags, fragments, edges, repo_root)
        return edges

    def _build_index(self, tf_frags: list[Fragment]) -> _TFIndex:
        idx = _TFIndex()
        for f in tf_frags:
            self._index_definitions(f, idx)
        return idx

    def _index_definitions(self, f: Fragment, idx: _TFIndex) -> None:
        for match in _TF_VARIABLE_RE.finditer(f.content):
            idx.var_defs[match.group(1)].append(f.id)

        for match in _TF_RESOURCE_RE.finditer(f.content):
            res_type, res_name = match.group(1), match.group(2)
            idx.resource_defs[f"{res_type}.{res_name}"].append(f.id)
            idx.resource_defs[res_name].append(f.id)

        for match in _TF_DATA_RE.finditer(f.content):
            data_type, data_name = match.group(1), match.group(2)
            idx.data_defs[f"{data_type}.{data_name}"].append(f.id)
            idx.data_defs[data_name].append(f.id)

        for local_key in _extract_locals(f.content):
            idx.local_defs[local_key].append(f.id)

        for match in _TF_MODULE_RE.finditer(f.content):
            idx.module_defs[match.group(1)].append(f.id)

    def _add_ref_edges(self, f: Fragment, idx: _TFIndex, edges: EdgeDict) -> None:
        self._add_var_edges(f, idx.var_defs, edges)
        self._add_local_edges(f, idx.local_defs, edges)
        self._add_data_edges(f, idx.data_defs, edges)
        self._add_module_edges(f, idx.module_defs, edges)
        self._add_resource_edges(f, idx.resource_defs, edges)

    def _add_var_edges(self, f: Fragment, var_defs: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        for match in _TF_VAR_REF_RE.finditer(f.content):
            for def_id in var_defs.get(match.group(1), []):
                if def_id != f.id:
                    self.add_edge(edges, f.id, def_id, self.weight)

    def _add_local_edges(self, f: Fragment, local_defs: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        for match in _TF_LOCAL_REF_RE.finditer(f.content):
            for def_id in local_defs.get(match.group(1), []):
                if def_id != f.id:
                    self.add_edge(edges, f.id, def_id, self.weight)

    def _add_data_edges(self, f: Fragment, data_defs: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        for match in _TF_DATA_REF_RE.finditer(f.content):
            data_type, data_name = match.group(1), match.group(2)
            full_ref = f"{data_type}.{data_name}"
            for def_id in data_defs.get(full_ref, []) + data_defs.get(data_name, []):
                if def_id != f.id:
                    self.add_edge(edges, f.id, def_id, self.weight)

    def _add_module_edges(self, f: Fragment, module_defs: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        for match in _TF_MODULE_REF_RE.finditer(f.content):
            for def_id in module_defs.get(match.group(1), []):
                if def_id != f.id:
                    self.add_edge(edges, f.id, def_id, self.weight)

    def _add_resource_edges(self, f: Fragment, resource_defs: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        skip_types = {"var", "local", "data", "module", "path", "terraform"}
        for match in _TF_RESOURCE_REF_RE.finditer(f.content):
            res_type, res_name, _ = match.groups()
            if res_type in skip_types:
                continue
            full_ref = f"{res_type}.{res_name}"
            for def_id in resource_defs.get(full_ref, []) + resource_defs.get(res_name, []):
                if def_id != f.id:
                    self.add_edge(edges, f.id, def_id, self.weight)

    def _build_module_source_edges(
        self,
        tf_frags: list[Fragment],
        all_frags: list[Fragment],
        edges: EdgeDict,
        repo_root: Path | None,
    ) -> None:
        path_to_frags = build_path_to_frags(all_frags, repo_root)

        for f in tf_frags:
            self._link_module_sources(f, path_to_frags, edges, repo_root)

    def _link_module_sources(
        self,
        f: Fragment,
        path_to_frags: dict[Path, list[FragmentId]],
        edges: EdgeDict,
        repo_root: Path | None,
    ) -> None:
        base_dir = f.path.parent

        for match in _TF_SOURCE_RE.finditer(f.content):
            source = match.group(1)
            if source.startswith("./") or source.startswith("../"):
                module_dir = (base_dir / source).resolve()
                self._link_files_in_module_dir(f, module_dir, path_to_frags, edges, repo_root)

    def _link_files_in_module_dir(
        self,
        f: Fragment,
        module_dir: Path,
        path_to_frags: dict[Path, list[FragmentId]],
        edges: EdgeDict,
        repo_root: Path | None,
    ) -> None:
        for p, frag_ids in path_to_frags.items():
            resolved = self._resolve_path(p, repo_root)
            if resolved is None:
                continue
            if self._is_in_module_dir(resolved, module_dir):
                for frag_id in frag_ids:
                    if frag_id != f.id:
                        self.add_edge(edges, f.id, frag_id, self.weight * 0.8)

    def _resolve_path(self, p: Path, repo_root: Path | None) -> Path | None:
        try:
            if p.is_absolute():
                return p.resolve()
            if repo_root:
                return (repo_root / p).resolve()
            return p
        except (ValueError, OSError):
            return None

    def _is_in_module_dir(self, resolved: Path, module_dir: Path) -> bool:
        try:
            return resolved.is_relative_to(module_dir)
        except (ValueError, TypeError):
            return False
