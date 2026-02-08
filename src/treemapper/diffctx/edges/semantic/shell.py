from __future__ import annotations

import re
from pathlib import Path

from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_SHELL_EXTS = {".sh", ".bash", ".zsh", ".ksh", ".fish"}
_POWERSHELL_EXTS = {".ps1", ".psm1", ".psd1"}
_ALL_SHELL = _SHELL_EXTS | _POWERSHELL_EXTS

_SOURCE_RE = re.compile(r"^\s{0,20}(?:source|\.)\s{1,10}['\"]?([^'\"#\s]{1,300})", re.MULTILINE)
_BASH_FUNC_RE = re.compile(r"^\s{0,20}(?:function\s{1,10})?(\w{1,100})\s{0,10}\(\s{0,10}\)", re.MULTILINE)

_SCRIPT_CALL_RE = re.compile(r"(?:bash|sh|zsh|python|python3|node|ruby|perl)\s{1,10}['\"]?([^\s'\"]{1,300})", re.MULTILINE)
_EXEC_CALL_RE = re.compile(r"(?:\./|scripts/|bin/)([a-zA-Z0-9_.-]{1,100}(?:\.(?:sh|py|rb|pl))?)", re.MULTILINE)
_ENV_FILE_RE = re.compile(r"^\s{0,20}(?:source|\.)\s{1,10}[^\n]{0,500}\.env", re.MULTILINE)

_PS_IMPORT_RE = re.compile(r"Import-Module\s+['\"]?([^\s'\"]+)", re.IGNORECASE)
_PS_DOT_SOURCE_RE = re.compile(r"\.\s+['\"]?([^\s'\"]+\.ps[m1d]?1)", re.IGNORECASE)
_PS_FUNC_RE = re.compile(r"^\s*function\s+(\w+[-\w]*)", re.MULTILINE | re.IGNORECASE)


def _is_shell_script(path: Path) -> bool:
    if path.suffix.lower() in _ALL_SHELL:
        return True
    name = path.name.lower()
    return name in {"bashrc", "bash_profile", "zshrc", "profile", "bash_aliases"}


def _is_powershell(path: Path) -> bool:
    return path.suffix.lower() in _POWERSHELL_EXTS


def _has_shebang(content: str) -> bool:
    first_line = content.split("\n")[0] if content else ""
    return first_line.startswith("#!") and any(sh in first_line for sh in ["bash", "sh", "zsh", "fish", "python", "ruby", "perl"])


def _extract_bash_refs(content: str) -> tuple[set[str], set[str]]:
    sourced: set[str] = set()
    scripts: set[str] = set()

    for match in _SOURCE_RE.finditer(content):
        path = match.group(1).strip()
        if not path.startswith("$"):
            sourced.add(path)

    scripts.update(_SCRIPT_CALL_RE.findall(content))
    scripts.update(_EXEC_CALL_RE.findall(content))

    return sourced, scripts


def _extract_ps_refs(content: str) -> tuple[set[str], set[str]]:
    imports: set[str] = set()
    scripts: set[str] = set()

    for match in _PS_IMPORT_RE.finditer(content):
        imports.add(match.group(1))

    for match in _PS_DOT_SOURCE_RE.finditer(content):
        scripts.add(match.group(1))

    return imports, scripts


def _extract_functions(content: str, is_ps: bool) -> set[str]:
    pattern = _PS_FUNC_RE if is_ps else _BASH_FUNC_RE
    return {m.group(1) for m in pattern.finditer(content)}


def _collect_shell_refs(shell_files: list[Path]) -> set[str]:
    refs: set[str] = set()
    for sf in shell_files:
        try:
            content = sf.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue

        if _is_powershell(sf):
            sourced, scripts = _extract_ps_refs(content)
        else:
            sourced, scripts = _extract_bash_refs(content)

        refs.update(sourced)
        refs.update(scripts)

        if _ENV_FILE_RE.search(content):
            refs.add(".env")

    return refs


class ShellEdgeBuilder(EdgeBuilder):
    weight = 0.55
    source_weight = 0.65
    script_weight = 0.55
    reverse_weight_factor = 0.35

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        shell_files = [f for f in changed_files if _is_shell_script(f)]
        if not shell_files:
            return []

        refs = _collect_shell_refs(shell_files)
        discovered = discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)
        changed_set = set(changed_files)
        env_files = [c for c in all_candidate_files if c.name.startswith(".env") and c not in changed_set]
        return list(set(discovered + env_files))

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        shell_frags = [f for f in fragments if _is_shell_script(f.path) or _has_shebang(f.content)]
        if not shell_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)

        for sf in shell_frags:
            self._add_fragment_edges(sf, idx, fragments, edges)

        return edges

    def _add_fragment_edges(self, sf: Fragment, idx: FragmentIndex, fragments: list[Fragment], edges: EdgeDict) -> None:
        is_ps = _is_powershell(sf.path)

        if is_ps:
            sourced, scripts = _extract_ps_refs(sf.content)
        else:
            sourced, scripts = _extract_bash_refs(sf.content)

        for src in sourced:
            self._link_ref(sf.id, src, idx, edges, self.source_weight)

        for script in scripts:
            self._link_ref(sf.id, script, idx, edges, self.script_weight)

        if _ENV_FILE_RE.search(sf.content):
            self._add_env_edges(sf.id, fragments, edges)

    def _add_env_edges(self, src_id: FragmentId, fragments: list[Fragment], edges: EdgeDict) -> None:
        for f in fragments:
            if f.path.name.lower().startswith(".env"):
                self.add_edge(edges, src_id, f.id, self.weight * 0.7)

    def _link_ref(
        self,
        src_id: FragmentId,
        ref: str,
        idx: FragmentIndex,
        edges: EdgeDict,
        weight: float,
    ) -> None:
        ref_name = ref.split("/")[-1].lower()
        if self._link_by_name(src_id, ref_name, idx, edges, weight):
            return
        self._link_by_path(src_id, ref, idx, edges, weight)

    def _link_by_name(
        self,
        src_id: FragmentId,
        ref_name: str,
        idx: FragmentIndex,
        edges: EdgeDict,
        weight: float,
    ) -> bool:
        ref_base = ref_name.split(".")[0] if ref_name else ""
        found = False
        for name, frag_ids in idx.by_name.items():
            if name == ref_name or (ref_base and name.startswith(ref_base)):
                for fid in frag_ids:
                    if fid != src_id:
                        self.add_edge(edges, src_id, fid, weight)
                        found = True
        return found

    def _link_by_path(
        self,
        src_id: FragmentId,
        ref: str,
        idx: FragmentIndex,
        edges: EdgeDict,
        weight: float,
    ) -> None:
        self.link_by_path_match(src_id, ref, idx, edges, weight)
