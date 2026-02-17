from __future__ import annotations

import re
from pathlib import Path

from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, build_path_to_frags, discover_files_by_refs

_DOCKERFILE_NAMES = {"dockerfile"}
_COMPOSE_NAMES = {"docker-compose.yml", "docker-compose.yaml", "compose.yml", "compose.yaml"}

_DOCKERFILE_FROM_RE = re.compile(r"^FROM\s+(\S+)", re.MULTILINE | re.IGNORECASE)
_DOCKERFILE_COPY_RE = re.compile(r"^(?:COPY|ADD)\s+(?:--[^\s]+\s+)*([^\s]+)\s+", re.MULTILINE | re.IGNORECASE)
_DOCKERFILE_ENV_RE = re.compile(r"^ENV\s+(\w+)", re.MULTILINE | re.IGNORECASE)
_DOCKERFILE_ARG_RE = re.compile(r"^ARG\s+(\w+)", re.MULTILINE | re.IGNORECASE)

_COMPOSE_BUILD_RE = re.compile(r"^\s+build:\s*['\"]?([^'\"#\n]+)", re.MULTILINE)
_COMPOSE_CONTEXT_RE = re.compile(r"^\s+context:\s*['\"]?([^'\"#\n]+)", re.MULTILINE)
_COMPOSE_DOCKERFILE_RE = re.compile(r"^\s+dockerfile:\s*['\"]?([^'\"#\n]+)", re.MULTILINE)
_COMPOSE_DEPENDS_RE = re.compile(r"depends_on:\s*\n((?:\s+-\s*\w+\s*\n)+)", re.MULTILINE)
_COMPOSE_SERVICE_RE = re.compile(r"^(\w+):\s*$", re.MULTILINE)
_COMPOSE_VOLUME_RE = re.compile(r"^\s+-\s*['\"]?([./][^:'\"\n]+):", re.MULTILINE)


def _is_dockerfile(path: Path) -> bool:
    name = path.name.lower()
    return name in _DOCKERFILE_NAMES or name.startswith("dockerfile.") or name.endswith(".dockerfile")


def _is_compose_file(path: Path) -> bool:
    return path.name.lower() in _COMPOSE_NAMES


def _strip_dot_slash(s: str) -> str:
    while s.startswith("./"):
        s = s[2:]
    return s


def _normalize_path(base_dir: Path, rel_path: str) -> Path:
    rel_path = rel_path.strip().strip("'\"")
    rel_path = _strip_dot_slash(rel_path)
    normalized = base_dir / rel_path
    if ".." in normalized.parts:
        return base_dir
    return normalized


def _collect_docker_refs(docker_files: list[Path]) -> set[str]:
    refs: set[str] = set()
    for df in docker_files:
        try:
            content = df.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue

        if _is_dockerfile(df):
            _collect_dockerfile_refs(content, refs)
        if _is_compose_file(df):
            _collect_compose_refs(content, refs)
    return refs


def _collect_dockerfile_refs(content: str, refs: set[str]) -> None:
    for match in _DOCKERFILE_COPY_RE.finditer(content):
        src = match.group(1)
        if not src.startswith("--") and not src.startswith("$"):
            refs.add(_strip_dot_slash(src.strip().strip("'\"")))


def _collect_compose_refs(content: str, refs: set[str]) -> None:
    for match in _COMPOSE_BUILD_RE.finditer(content):
        refs.add(_strip_dot_slash(match.group(1).strip()))
    for match in _COMPOSE_VOLUME_RE.finditer(content):
        refs.add(_strip_dot_slash(match.group(1).strip()))


class DockerEdgeBuilder(EdgeBuilder):
    weight = 0.55
    copy_weight = 0.65
    compose_weight = 0.50
    reverse_weight_factor = 0.40

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        docker_files = [f for f in changed_files if _is_dockerfile(f) or _is_compose_file(f)]
        if not docker_files:
            return []

        refs = _collect_docker_refs(docker_files)
        return discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        edges: EdgeDict = {}

        dockerfiles = [f for f in fragments if _is_dockerfile(f.path)]
        compose_files = [f for f in fragments if _is_compose_file(f.path)]

        if not dockerfiles and not compose_files:
            return edges

        path_to_frags = build_path_to_frags(fragments, repo_root)

        self._build_dockerfile_edges(dockerfiles, path_to_frags, edges)
        self._build_compose_edges(compose_files, dockerfiles, path_to_frags, edges)

        return edges

    def _build_dockerfile_edges(
        self,
        dockerfiles: list[Fragment],
        path_to_frags: dict[Path, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for df in dockerfiles:
            self._add_copy_edges(df, path_to_frags, edges)
            self._add_env_edges(df, path_to_frags, edges)

    def _add_copy_edges(self, df: Fragment, path_to_frags: dict[Path, list[FragmentId]], edges: EdgeDict) -> None:
        base_dir = df.path.parent
        for match in _DOCKERFILE_COPY_RE.finditer(df.content):
            src_path = match.group(1)
            if src_path.startswith("--") or src_path.startswith("$"):
                continue
            self._link_copy_source(df.id, base_dir, src_path, path_to_frags, edges)

    def _link_copy_source(
        self,
        df_id: FragmentId,
        base_dir: Path,
        src_path: str,
        path_to_frags: dict[Path, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        target = _normalize_path(base_dir, src_path)
        for frag_id in path_to_frags.get(target, []):
            self.add_edge(edges, df_id, frag_id, self.copy_weight)

        if "*" in src_path:
            return
        suffix = _strip_dot_slash(src_path)
        if not suffix or suffix == ".":
            return
        for p, frag_ids in path_to_frags.items():
            if str(p).endswith(suffix):
                for frag_id in frag_ids:
                    self.add_edge(edges, df_id, frag_id, self.copy_weight * 0.8)

    def _add_env_edges(self, df: Fragment, path_to_frags: dict[Path, list[FragmentId]], edges: EdgeDict) -> None:
        env_vars = set(_DOCKERFILE_ENV_RE.findall(df.content))
        arg_vars = set(_DOCKERFILE_ARG_RE.findall(df.content))
        if not (env_vars or arg_vars):
            return

        for p in path_to_frags:
            if p.suffix.lower() == ".env" or p.name.lower().startswith(".env"):
                for frag_id in path_to_frags[p]:
                    self.add_edge(edges, df.id, frag_id, self.weight)

    def _build_compose_edges(
        self,
        compose_files: list[Fragment],
        dockerfiles: list[Fragment],
        path_to_frags: dict[Path, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for cf in compose_files:
            self._link_nearby_dockerfiles(cf, dockerfiles, edges)
            self._link_compose_build(cf, path_to_frags, edges)
            self._link_compose_context(cf, path_to_frags, edges)
            self._link_compose_volumes(cf, path_to_frags, edges)

    def _link_nearby_dockerfiles(self, cf: Fragment, dockerfiles: list[Fragment], edges: EdgeDict) -> None:
        base_dir = cf.path.parent
        for df in dockerfiles:
            if df.path.parent == base_dir or df.path.parent.parent == base_dir:
                self.add_edge(edges, cf.id, df.id, self.compose_weight)

    def _link_compose_build(self, cf: Fragment, path_to_frags: dict[Path, list[FragmentId]], edges: EdgeDict) -> None:
        base_dir = cf.path.parent
        for match in _COMPOSE_BUILD_RE.finditer(cf.content):
            build_path = match.group(1).strip()
            if not build_path or build_path.startswith("$"):
                continue
            dockerfile_path = _normalize_path(base_dir, build_path) / "Dockerfile"
            for frag_id in path_to_frags.get(dockerfile_path, []):
                self.add_edge(edges, cf.id, frag_id, self.compose_weight)

    def _link_compose_context(self, cf: Fragment, path_to_frags: dict[Path, list[FragmentId]], edges: EdgeDict) -> None:
        base_dir = cf.path.parent
        for match in _COMPOSE_CONTEXT_RE.finditer(cf.content):
            context_path = match.group(1).strip()
            if not context_path or context_path.startswith("$"):
                continue
            target_dir = _normalize_path(base_dir, context_path)
            self._link_context_files(cf.id, target_dir, path_to_frags, edges)

    def _link_context_files(
        self,
        cf_id: FragmentId,
        target_dir: Path,
        path_to_frags: dict[Path, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for p, frag_ids in path_to_frags.items():
            try:
                if p.is_relative_to(target_dir) or target_dir.is_relative_to(p.parent):
                    for frag_id in frag_ids:
                        self.add_edge(edges, cf_id, frag_id, self.compose_weight * 0.7)
            except (ValueError, TypeError):
                continue

    def _link_compose_volumes(self, cf: Fragment, path_to_frags: dict[Path, list[FragmentId]], edges: EdgeDict) -> None:
        base_dir = cf.path.parent
        for match in _COMPOSE_VOLUME_RE.finditer(cf.content):
            vol_path = match.group(1).strip()
            if vol_path:
                target = _normalize_path(base_dir, vol_path)
                for frag_id in path_to_frags.get(target, []):
                    self.add_edge(edges, cf.id, frag_id, self.compose_weight * 0.6)
