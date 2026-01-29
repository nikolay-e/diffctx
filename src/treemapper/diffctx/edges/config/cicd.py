from __future__ import annotations

import configparser
import logging
import re
from pathlib import Path

from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_GHA_RUN_RE = re.compile(r"^\s{0,20}-?\s{0,5}run:\s{0,5}[|>]?\s{0,5}([^\n]{1,500})", re.MULTILINE)

_GITLAB_SCRIPT_RE = re.compile(
    r"^\s{0,20}(?:script|before_script|after_script):\s?\n((?:\s{1,20}-\s{0,5}[^\n]{1,500}\n){1,100})", re.MULTILINE
)
_GITLAB_INCLUDE_RE = re.compile(
    r"^\s{0,20}-?\s{0,5}(?:local|project|remote|template):\s{0,5}['\"]?([^'\"#\n]{1,300})", re.MULTILINE
)

_JENKINS_SH_RE = re.compile(r"sh\s*(?:\(['\"]|['\"])(.+?)['\"]\)?", re.MULTILINE | re.DOTALL)
_JENKINS_SCRIPT_RE = re.compile(r"script\s*\{([^}]+)\}", re.MULTILINE | re.DOTALL)

_SCRIPT_CALL_RE = re.compile(
    r"(?:bash|sh|python|python3|node|npm|yarn|pnpm|make|go|cargo|dotnet|mvn|gradle|pytest|ruff|mypy|black|isort|flake8)\s+([^\s;&|]+)"
)
_FILE_REF_RE = re.compile(r"(?:\.\/|scripts\/|bin\/|tools\/|src\/|tests\/)([a-zA-Z0-9_.-]+(?:\.(?:sh|py|js|ts|rb))?)")


def _is_github_actions(path: Path) -> bool:
    parts = path.parts
    return ".github" in parts and "workflows" in parts and path.suffix.lower() in {".yml", ".yaml"}


def _is_gitlab_ci(path: Path) -> bool:
    return path.name.lower() in {".gitlab-ci.yml", ".gitlab-ci.yaml"} or path.name.lower().startswith("gitlab-ci")


def _is_jenkinsfile(path: Path) -> bool:
    name = path.name.lower()
    return name == "jenkinsfile" or name.endswith(".jenkinsfile") or name.endswith(".jenkins")


def _is_circleci(path: Path) -> bool:
    return ".circleci" in path.parts and path.name.lower() in {"config.yml", "config.yaml"}


def _is_travis(path: Path) -> bool:
    return path.name.lower() in {".travis.yml", ".travis.yaml"}


def _is_azure_pipelines(path: Path) -> bool:
    name = path.name.lower()
    return name in {"azure-pipelines.yml", "azure-pipelines.yaml"} or name.startswith("azure-pipeline")


def _is_tox(path: Path) -> bool:
    return path.name.lower() == "tox.ini"


def _is_nox(path: Path) -> bool:
    return path.name.lower() == "noxfile.py"


def _is_ci_file(path: Path) -> bool:
    return any(
        [
            _is_github_actions(path),
            _is_gitlab_ci(path),
            _is_jenkinsfile(path),
            _is_circleci(path),
            _is_travis(path),
            _is_azure_pipelines(path),
            _is_tox(path),
            _is_nox(path),
        ]
    )


def _extract_script_refs(content: str) -> set[str]:
    refs: set[str] = set()

    for match in _SCRIPT_CALL_RE.finditer(content):
        script = match.group(1).strip().strip("'\"")
        if script and not script.startswith("-"):
            refs.add(script)

    for match in _FILE_REF_RE.finditer(content):
        refs.add(match.group(1))

    return refs


def _extract_gha_refs(content: str) -> set[str]:
    refs: set[str] = set()

    for match in _GHA_RUN_RE.finditer(content):
        run_content = match.group(1)
        refs.update(_extract_script_refs(run_content))

    return refs


def _extract_gitlab_refs(content: str) -> set[str]:
    refs: set[str] = set()

    for match in _GITLAB_SCRIPT_RE.finditer(content):
        script_block = match.group(1)
        refs.update(_extract_script_refs(script_block))

    for match in _GITLAB_INCLUDE_RE.finditer(content):
        refs.add(match.group(1).strip())

    return refs


def _extract_jenkins_refs(content: str) -> set[str]:
    refs: set[str] = set()

    for match in _JENKINS_SH_RE.finditer(content):
        sh_content = match.group(1)
        refs.update(_extract_script_refs(sh_content))

    for match in _JENKINS_SCRIPT_RE.finditer(content):
        refs.update(_extract_script_refs(match.group(1)))

    return refs


def _extract_tox_refs(content: str) -> set[str]:
    refs: set[str] = set()
    parser = configparser.ConfigParser()
    try:
        parser.read_string(content)
    except configparser.Error:
        return refs

    for section in parser.sections():
        if not section.lower().startswith("testenv"):
            continue

        deps = parser.get(section, "deps", fallback="")
        commands = parser.get(section, "commands", fallback="")

        for line in deps.splitlines():
            line = line.strip()
            if not line or line.startswith("#") or line.startswith("-r"):
                continue
            refs.add(line.split()[0] if line.split() else line)

        for line in commands.splitlines():
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split()
            for p in parts:
                p = p.strip("'\"")
                p = re.sub(r"\{[^}]+\}", "", p)
                if not p or p.startswith("-"):
                    continue
                refs.add(p)
            refs.update(_extract_script_refs(line))

    return refs


def _extract_nox_refs(content: str) -> set[str]:
    refs: set[str] = set()
    # Extract session.run calls
    # session.run("cmd", "arg1", ...)
    for match in re.finditer(r"session\.run\(([^)]+)\)", content):
        args_str = match.group(1)
        # Simple extraction of string literals
        args = re.findall(r'["\']([^"\']+)["\']', args_str)
        for arg in args:
            refs.add(arg)
            refs.update(_extract_script_refs(arg))

    # Extract session.install calls
    for match in re.finditer(r"session\.install\(([^)]+)\)", content):
        args_str = match.group(1)
        args = re.findall(r'["\']([^"\']+)["\']', args_str)
        for arg in args:
            refs.add(arg)

    return refs


class CICDEdgeBuilder(EdgeBuilder):
    weight = 0.55
    script_weight = 0.60
    reverse_weight_factor = 0.35

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        ci_files = [f for f in changed_files if _is_ci_file(f)]
        if not ci_files:
            return []

        refs: set[str] = set()

        for ci in ci_files:
            try:
                content = ci.read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                continue

            local_refs = set()
            if _is_github_actions(ci):
                local_refs.update(_extract_gha_refs(content))
            elif _is_gitlab_ci(ci):
                local_refs.update(_extract_gitlab_refs(content))
            elif _is_jenkinsfile(ci):
                local_refs.update(_extract_jenkins_refs(content))
            elif _is_tox(ci):
                local_refs.update(_extract_tox_refs(content))
            elif _is_nox(ci):
                local_refs.update(_extract_nox_refs(content))
            else:
                local_refs.update(_extract_script_refs(content))

            if any(cmd in content.lower() for cmd in ["npm", "yarn", "pnpm"]):
                local_refs.add("package.json")

            logging.debug("CICD refs for %s: %s", ci.name, local_refs)
            refs.update(local_refs)

        discovered = discover_files_by_refs(refs, changed_files, all_candidate_files)
        logging.debug("CICD discovered for %s: %s", [c.name for c in ci_files], [d.name for d in discovered])
        return discovered

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        ci_frags = [f for f in fragments if _is_ci_file(f.path)]
        if not ci_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)

        for ci in ci_frags:
            refs = self._extract_refs(ci)
            self._link_refs(ci.id, refs, idx, edges)
            self._link_to_package_json(ci, fragments, edges)

        return edges

    def _extract_refs(self, ci: Fragment) -> set[str]:
        if _is_github_actions(ci.path):
            return _extract_gha_refs(ci.content)
        if _is_gitlab_ci(ci.path):
            return _extract_gitlab_refs(ci.content)
        if _is_jenkinsfile(ci.path):
            return _extract_jenkins_refs(ci.content)
        if _is_tox(ci.path):
            return _extract_tox_refs(ci.content)
        if _is_nox(ci.path):
            return _extract_nox_refs(ci.content)
        return _extract_script_refs(ci.content)

    def _link_refs(self, ci_id: FragmentId, refs: set[str], idx: FragmentIndex, edges: EdgeDict) -> None:
        for ref in refs:
            self._link_by_name(ci_id, ref, idx, edges)
            self._link_by_path(ci_id, ref, idx, edges)

    def _link_by_name(self, ci_id: FragmentId, ref: str, idx: FragmentIndex, edges: EdgeDict) -> None:
        ref_name = ref.split("/")[-1].lower()
        ref_base = ref_name.split(".")[0]
        for name, frag_ids in idx.by_name.items():
            if name == ref_name or name.startswith(ref_base):
                for fid in frag_ids:
                    if fid != ci_id:
                        self.add_edge(edges, ci_id, fid, self.script_weight)

    def _link_by_path(self, ci_id: FragmentId, ref: str, idx: FragmentIndex, edges: EdgeDict) -> None:
        self.link_by_path_match(ci_id, ref, idx, edges, self.script_weight)

    def _link_to_package_json(self, ci_frag: Fragment, all_frags: list[Fragment], edges: EdgeDict) -> None:
        npm_commands = {"npm", "yarn", "pnpm", "npx"}
        if not any(cmd in ci_frag.content.lower() for cmd in npm_commands):
            return

        for f in all_frags:
            if f.path.name.lower() == "package.json":
                self.add_edge(edges, ci_frag.id, f.id, self.weight * 0.8)
