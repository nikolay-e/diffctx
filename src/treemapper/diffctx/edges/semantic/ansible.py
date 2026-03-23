from __future__ import annotations

import re
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_ANSIBLE_EXTS = {".yml", ".yaml"}
_TEMPLATE_EXTS = {".j2", ".jinja2", ".jinja"}

_INCLUDE_VARS_RE = re.compile(
    r"^\s*(?:include_vars|vars_files)\s*:\s*[\"']?([^\s\"']{1,300})[\"']?",
    re.MULTILINE,
)
_INCLUDE_TASKS_RE = re.compile(
    r"^\s*(?:include_tasks|import_tasks|include_role|import_role|import_playbook)\s*:\s*[\"']?([^\s\"']{1,300})[\"']?",
    re.MULTILINE,
)
_TEMPLATE_SRC_RE = re.compile(
    r"^\s*src\s*:\s*[\"']?([^\s\"']{1,300}\.j(?:2|inja2?))[\"']?",
    re.MULTILINE,
)
_ROLES_LIST_RE = re.compile(
    r"^\s*-\s+(?:role:\s*)?([a-zA-Z_][\w.-]{0,200})\s*$",
    re.MULTILINE,
)
_VARS_FILES_LIST_RE = re.compile(
    r"^\s*-\s+[\"']([^\"']{1,300}\.ya?ml)[\"']",
    re.MULTILINE,
)


def _is_ansible_file(path: Path) -> bool:
    return path.suffix.lower() in _ANSIBLE_EXTS or path.suffix.lower() in _TEMPLATE_EXTS


def _get_role_name_from_path(path: Path) -> str | None:
    parts = path.parts
    for i, part in enumerate(parts):
        if part == "roles" and i + 1 < len(parts):
            return parts[i + 1]
    return None


def _extract_refs(content: str, file_path: Path) -> set[str]:
    refs: set[str] = set()

    for m in _INCLUDE_VARS_RE.finditer(content):
        refs.add(m.group(1))
    for m in _INCLUDE_TASKS_RE.finditer(content):
        refs.add(m.group(1))
    for m in _TEMPLATE_SRC_RE.finditer(content):
        refs.add(m.group(1))
    for m in _VARS_FILES_LIST_RE.finditer(content):
        refs.add(m.group(1))

    role_name = _get_role_name_from_path(file_path)
    if role_name and "/tasks/" in str(file_path):
        refs.add(f"roles/{role_name}/handlers/main.yml")
        refs.add(f"roles/{role_name}/templates/")
        refs.add(f"roles/{role_name}/files/")

    return refs


def _is_roles_block_terminator(stripped: str) -> bool:
    return bool(stripped) and not stripped.startswith("#") and not stripped.startswith("-") and ":" in stripped


def _extract_role_refs(content: str) -> set[str]:
    roles: set[str] = set()
    in_roles_block = False
    for line in content.splitlines():
        stripped = line.strip()
        if stripped.startswith("roles:"):
            in_roles_block = True
            continue
        if not in_roles_block:
            continue
        if stripped.startswith("- "):
            m = _ROLES_LIST_RE.match(line)
            if m:
                roles.add(m.group(1))
        elif _is_roles_block_terminator(stripped):
            in_roles_block = False
    return roles


def _ref_to_filename(ref: str) -> str:
    name = ref.rstrip("/").split("/")[-1].lower()
    return name


class AnsibleEdgeBuilder(EdgeBuilder):
    weight = 0.60
    include_weight = EDGE_WEIGHTS["ansible_include"].forward
    role_weight = EDGE_WEIGHTS["ansible_role"].forward
    reverse_weight_factor = EDGE_WEIGHTS["ansible_include"].reverse_factor

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        ansible_changed = [f for f in changed_files if _is_ansible_file(f)]
        if not ansible_changed:
            return []

        refs: set[str] = set()
        role_names: set[str] = set()

        for f in ansible_changed:
            try:
                content = f.read_text(encoding="utf-8")
                for ref in _extract_refs(content, f):
                    refs.add(_ref_to_filename(ref))
                    refs.add(ref)
                for role in _extract_role_refs(content):
                    role_names.add(role)
            except (OSError, UnicodeDecodeError):
                continue

            role_name = _get_role_name_from_path(f)
            if role_name:
                role_names.add(role_name)

        for role in role_names:
            refs.add(f"roles/{role}/tasks/main.yml")
            refs.add(f"roles/{role}/handlers/main.yml")
            refs.add(f"roles/{role}/templates/")

        self._add_group_vars_refs(ansible_changed, all_candidate_files, refs, repo_root)

        return discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

    def _scan_group_host_vars_refs(
        self,
        f: Path,
        all_candidate_files: list[Path],
        refs: set[str],
    ) -> None:
        fname = f.name
        fstr = str(f)
        for candidate in all_candidate_files:
            try:
                content = candidate.read_text(encoding="utf-8")
                if fname in content or fstr in content:
                    refs.add(candidate.name.lower())
            except (OSError, UnicodeDecodeError):
                continue

    def _scan_vars_file_refs(self, f: Path, refs: set[str]) -> None:
        try:
            content = f.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            return
        if "vars_files:" not in content and "include_vars" not in content:
            return
        for m in _VARS_FILES_LIST_RE.finditer(content):
            refs.add(m.group(1))
        for m in _INCLUDE_VARS_RE.finditer(content):
            refs.add(m.group(1))

    def _add_group_vars_refs(
        self,
        ansible_changed: list[Path],
        all_candidate_files: list[Path],
        refs: set[str],
        _repo_root: Path | None,
    ) -> None:
        for f in ansible_changed:
            rel = str(f)
            if "group_vars/" in rel or "host_vars/" in rel:
                self._scan_group_host_vars_refs(f, all_candidate_files, refs)
            self._scan_vars_file_refs(f, refs)

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        ansible_frags = [f for f in fragments if _is_ansible_file(f.path)]
        if not ansible_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)

        for af in ansible_frags:
            self._add_fragment_edges(af, idx, edges)

        self._link_role_siblings(ansible_frags, edges)

        return edges

    def _add_fragment_edges(self, af: Fragment, idx: FragmentIndex, edges: EdgeDict) -> None:
        refs = _extract_refs(af.content, af.path)
        for ref in refs:
            self.link_by_name_or_path(af.id, ref, idx, edges, self.include_weight, filename=_ref_to_filename(ref))

        roles = _extract_role_refs(af.content)
        for role in roles:
            for subdir in ("tasks", "handlers", "templates"):
                path_hint = f"roles/{role}/{subdir}"
                self.link_by_path_match(af.id, path_hint, idx, edges, self.role_weight)

    def _link_role_siblings(self, ansible_frags: list[Fragment], edges: EdgeDict) -> None:
        role_frags: dict[str, list[Fragment]] = {}
        for f in ansible_frags:
            role = _get_role_name_from_path(f.path)
            if role:
                role_frags.setdefault(role, []).append(f)

        sibling_weight = self.weight * 0.6
        for frags in role_frags.values():
            for i, f1 in enumerate(frags):
                for f2 in frags[i + 1 :]:
                    self.add_edge(edges, f1.id, f2.id, sibling_weight)
