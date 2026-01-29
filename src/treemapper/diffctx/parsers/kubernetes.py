from __future__ import annotations

import logging
import re
from pathlib import Path

from ..types import Fragment
from .base import YAML_EXTENSIONS, create_fragment_from_lines

_K8S_DIRS = frozenset(
    {
        "k8s",
        "kubernetes",
        "helm",
        "charts",
        "manifests",
        "deploy",
        "deployment",
        "deployments",
        "base",
        "overlays",
        "templates",
        "kustomize",
    }
)

_APIVERSION_RE = re.compile(r"^apiVersion:\s*\S+", re.MULTILINE)
_KIND_RE = re.compile(r"^kind:\s*\S+", re.MULTILINE)


def _is_k8s_path(path: Path) -> bool:
    for part in path.parts:
        if part.lower() in _K8S_DIRS:
            return True
    return False


def _is_k8s_content(content: str) -> bool:
    return bool(_APIVERSION_RE.search(content) and _KIND_RE.search(content))


class KubernetesYamlStrategy:
    priority = 55

    def can_handle(self, path: Path, content: str) -> bool:
        if path.suffix.lower() not in YAML_EXTENSIONS:
            return False
        if not _is_k8s_content(content):
            return False
        return _is_k8s_path(path)

    def fragment(self, path: Path, content: str) -> list[Fragment]:
        lines = content.splitlines()
        if not lines:
            return []

        doc_starts = self._find_document_starts(lines)
        if not doc_starts:
            doc_starts = [0]

        fragments: list[Fragment] = []
        for i, start_idx in enumerate(doc_starts):
            if i + 1 < len(doc_starts):
                end_idx = doc_starts[i + 1] - 1
                while end_idx > start_idx and not lines[end_idx].strip():
                    end_idx -= 1
            else:
                end_idx = len(lines) - 1

            if end_idx < start_idx:
                continue

            frag = create_fragment_from_lines(path, lines, start_idx + 1, end_idx + 1, "resource", "config")
            if frag:
                fragments.append(frag)
                logging.debug("K8s fragment: %s lines %d-%d", path, start_idx + 1, end_idx + 1)

        return fragments

    def _find_document_starts(self, lines: list[str]) -> list[int]:
        starts: list[int] = []
        for i, line in enumerate(lines):
            if line.strip() == "---":
                if i + 1 < len(lines):
                    starts.append(i + 1)
            elif i == 0 and line.strip().startswith("apiVersion:"):
                starts.append(0)
        return starts
