from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict

_K8S_API_VERSION_RE = re.compile(r"^apiVersion:\s?([^\s#]{1,100})", re.MULTILINE)
_K8S_KIND_RE = re.compile(r"^kind:\s?(\w{1,100})", re.MULTILINE)
_K8S_NAME_RE = re.compile(r"^\s{1,20}name:\s?['\"]?([^'\"#\n]{1,200})", re.MULTILINE)
_K8S_NAMESPACE_RE = re.compile(r"^\s{1,20}namespace:\s?['\"]?([^'\"#\n]{1,200})", re.MULTILINE)

_CONFIGMAP_REF_RE = re.compile(r"configMapKeyRef:\s?\n\s{1,20}name:\s?['\"]?([^'\"#\n]{1,200})", re.MULTILINE)
_CONFIGMAP_NAME_RE = re.compile(r"configMapName:\s?['\"]?([^'\"#\n]{1,200})", re.MULTILINE)
_SECRET_REF_RE = re.compile(r"secretKeyRef:\s?\n\s{1,20}name:\s?['\"]?([^'\"#\n]{1,200})", re.MULTILINE)
_SECRET_NAME_RE = re.compile(r"secretName:\s?['\"]?([^'\"#\n]{1,200})", re.MULTILINE)

_SERVICE_NAME_RE = re.compile(r"serviceName:\s?['\"]?([^'\"#\n]{1,200})", re.MULTILINE)
_BACKEND_SERVICE_RE = re.compile(r"service:\s?\n\s{1,20}name:\s?['\"]?([^'\"#\n]{1,200})", re.MULTILINE)

_IMAGE_RE = re.compile(r"^\s{1,20}image:\s?['\"]?([^'\"#\n]{1,300})", re.MULTILINE)

_SELECTOR_MATCH_LABELS_RE = re.compile(
    r"selector:\s?\n\s{1,20}matchLabels:\s?\n((?:\s{1,20}[a-zA-Z0-9_./-]{1,100}:\s?[^\n:]{1,200}\n){1,50})", re.MULTILINE
)
_LABELS_RE = re.compile(r"labels:\s?\n((?:\s{1,20}[a-zA-Z0-9_./-]{1,100}:\s?[^\n:]{1,200}\n){1,50})", re.MULTILINE)
_LABEL_PAIR_RE = re.compile(r"^\s{0,20}([a-zA-Z0-9_./-]{1,100}):\s?['\"]?([a-zA-Z0-9_./-]{1,100})['\"]?\s{0,10}$", re.MULTILINE)
_SIMPLE_SELECTOR_RE = re.compile(r"selector:\s?\n((?:\s{1,20}[a-zA-Z0-9_./-]{1,100}:\s?[^\n:]{1,200}\n){1,50})", re.MULTILINE)

_VOLUME_CONFIGMAP_RE = re.compile(r"configMap:\s?\n\s{1,20}name:\s?['\"]?([^'\"#\n]{1,200})", re.MULTILINE)
_VOLUME_SECRET_RE = re.compile(r"secret:\s?\n\s{1,20}secretName:\s?['\"]?([^'\"#\n]{1,200})", re.MULTILINE)
_VOLUME_PVC_RE = re.compile(r"persistentVolumeClaim:\s?\n\s{1,20}claimName:\s?['\"]?([^'\"#\n]{1,200})", re.MULTILINE)

_YAML_EXTS = {".yaml", ".yml"}

_K8S_KINDS = {
    "Deployment",
    "Service",
    "ConfigMap",
    "Secret",
    "Ingress",
    "Pod",
    "ReplicaSet",
    "StatefulSet",
    "DaemonSet",
    "Job",
    "CronJob",
    "PersistentVolume",
    "PersistentVolumeClaim",
    "ServiceAccount",
    "Role",
    "RoleBinding",
    "ClusterRole",
    "ClusterRoleBinding",
    "NetworkPolicy",
    "HorizontalPodAutoscaler",
    "Namespace",
}


def _is_kubernetes_manifest(path: Path, content: str | None = None) -> bool:
    if path.suffix.lower() not in _YAML_EXTS:
        return False

    if content is None:
        try:
            content = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            return False

    api_match = _K8S_API_VERSION_RE.search(content)
    kind_match = _K8S_KIND_RE.search(content)

    if api_match and kind_match:
        kind = kind_match.group(1).strip()
        return kind in _K8S_KINDS

    return False


def _extract_resource_info(content: str) -> tuple[str | None, str | None, str | None]:
    kind_match = _K8S_KIND_RE.search(content)
    name_match = _K8S_NAME_RE.search(content)
    namespace_match = _K8S_NAMESPACE_RE.search(content)

    kind = kind_match.group(1).strip() if kind_match else None
    name = name_match.group(1).strip() if name_match else None
    namespace = namespace_match.group(1).strip() if namespace_match else None

    return kind, name, namespace


def _extract_label_pairs(label_block: str) -> dict[str, str]:
    return {m.group(1).strip(): m.group(2).strip() for m in _LABEL_PAIR_RE.finditer(label_block)}


def _extract_labels_by_pattern(content: str, pattern: re.Pattern[str]) -> dict[str, str]:
    labels: dict[str, str] = {}
    for match in pattern.finditer(content):
        labels.update(_extract_label_pairs(match.group(1)))
    return labels


def _extract_labels(content: str) -> dict[str, str]:
    return _extract_labels_by_pattern(content, _LABELS_RE)


def _extract_selector_labels(content: str) -> dict[str, str]:
    return _extract_labels_by_pattern(content, _SELECTOR_MATCH_LABELS_RE)


def _labels_match(selector: dict[str, str], labels: dict[str, str]) -> bool:
    if not selector:
        return False
    return all(labels.get(k) == v for k, v in selector.items())


def _find_k8s_files(changed_files: list[Path]) -> list[Path]:
    k8s_files: list[Path] = []
    for f in changed_files:
        if f.suffix.lower() not in _YAML_EXTS:
            continue
        try:
            content = f.read_text(encoding="utf-8")
            if _is_kubernetes_manifest(f, content):
                k8s_files.append(f)
        except (OSError, UnicodeDecodeError):
            continue
    return k8s_files


def _collect_k8s_dirs(k8s_files: list[Path]) -> set[Path]:
    k8s_dirs: set[Path] = set()
    for f in k8s_files:
        k8s_dirs.add(f.parent)
        if f.parent.name in {"base", "overlays", "templates", "manifests"}:
            k8s_dirs.add(f.parent.parent)
    return k8s_dirs


def _is_in_k8s_dir(candidate: Path, k8s_dirs: set[Path]) -> bool:
    for k8s_dir in k8s_dirs:
        try:
            if candidate.is_relative_to(k8s_dir):
                return True
        except (ValueError, TypeError):
            continue
    return False


class _K8sIndex:
    configmaps: dict[str, list[FragmentId]]
    secrets: dict[str, list[FragmentId]]
    services: dict[str, list[FragmentId]]
    pvcs: dict[str, list[FragmentId]]
    pods_with_labels: list[tuple[FragmentId, dict[str, str]]]
    images: dict[str, list[FragmentId]]

    def __init__(self) -> None:
        self.configmaps = defaultdict(list)
        self.secrets = defaultdict(list)
        self.services = defaultdict(list)
        self.pvcs = defaultdict(list)
        self.pods_with_labels = []
        self.images = defaultdict(list)


class KubernetesEdgeBuilder(EdgeBuilder):
    weight = 0.65
    configmap_secret_weight = 0.70
    service_weight = 0.60
    selector_weight = 0.55
    image_weight = 0.40
    reverse_weight_factor = 0.45

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        k8s_files = _find_k8s_files(changed_files)
        if not k8s_files:
            return []

        k8s_dirs = _collect_k8s_dirs(k8s_files)
        changed_set = set(changed_files)
        discovered: list[Path] = []

        for candidate in all_candidate_files:
            if candidate in changed_set or candidate.suffix.lower() not in _YAML_EXTS:
                continue
            if not _is_in_k8s_dir(candidate, k8s_dirs):
                continue
            try:
                content = candidate.read_text(encoding="utf-8")
                if _is_kubernetes_manifest(candidate, content):
                    discovered.append(candidate)
            except (OSError, UnicodeDecodeError):
                continue

        return discovered

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        k8s_fragments = [f for f in fragments if _is_kubernetes_manifest(f.path, f.content)]

        if not k8s_fragments:
            return {}

        edges: EdgeDict = {}
        idx = self._build_resource_index(k8s_fragments)

        for frag in k8s_fragments:
            self._build_configmap_edges(frag, idx.configmaps, edges)
            self._build_secret_edges(frag, idx.secrets, edges)
            self._build_service_edges(frag, idx.services, edges)
            self._build_volume_edges(frag, idx.pvcs, edges)
            self._build_selector_edges(frag, idx.pods_with_labels, edges)
            self._build_image_edges(frag, idx.images, edges)

        return edges

    def _build_resource_index(self, k8s_fragments: list[Fragment]) -> _K8sIndex:
        idx = _K8sIndex()
        for frag in k8s_fragments:
            self._index_fragment(frag, idx)
        return idx

    def _index_fragment(self, frag: Fragment, idx: _K8sIndex) -> None:
        kind, name, _ = _extract_resource_info(frag.content)

        if name:
            self._index_by_kind(kind, name, frag.id, idx)

        if kind in {"Pod", "Deployment", "StatefulSet", "DaemonSet", "ReplicaSet", "Job", "CronJob"}:
            labels = _extract_labels(frag.content)
            if labels:
                idx.pods_with_labels.append((frag.id, labels))

        self._index_images(frag, idx)

    def _index_by_kind(self, kind: str | None, name: str, frag_id: FragmentId, idx: _K8sIndex) -> None:
        if kind == "ConfigMap":
            idx.configmaps[name].append(frag_id)
        elif kind == "Secret":
            idx.secrets[name].append(frag_id)
        elif kind == "Service":
            idx.services[name].append(frag_id)
        elif kind == "PersistentVolumeClaim":
            idx.pvcs[name].append(frag_id)

    def _index_images(self, frag: Fragment, idx: _K8sIndex) -> None:
        for match in _IMAGE_RE.finditer(frag.content):
            image = match.group(1).strip()
            if image and not image.startswith("$"):
                idx.images[image].append(frag.id)

    def _link_by_patterns(
        self,
        frag: Fragment,
        patterns: list[re.Pattern[str]],
        index: dict[str, list[FragmentId]],
        edges: EdgeDict,
        weight: float,
    ) -> None:
        for pattern in patterns:
            for match in pattern.finditer(frag.content):
                name = match.group(1).strip()
                for target_id in index.get(name, []):
                    if target_id != frag.id:
                        self.add_edge(edges, frag.id, target_id, weight)

    def _build_configmap_edges(
        self,
        frag: Fragment,
        configmaps: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        patterns = [_CONFIGMAP_REF_RE, _CONFIGMAP_NAME_RE, _VOLUME_CONFIGMAP_RE]
        self._link_by_patterns(frag, patterns, configmaps, edges, self.configmap_secret_weight)

    def _build_secret_edges(
        self,
        frag: Fragment,
        secrets: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        patterns = [_SECRET_REF_RE, _SECRET_NAME_RE, _VOLUME_SECRET_RE]
        self._link_by_patterns(frag, patterns, secrets, edges, self.configmap_secret_weight)

    def _build_service_edges(
        self,
        frag: Fragment,
        services: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        patterns = [_SERVICE_NAME_RE, _BACKEND_SERVICE_RE]
        self._link_by_patterns(frag, patterns, services, edges, self.service_weight)

    def _build_volume_edges(
        self,
        frag: Fragment,
        pvcs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        self._link_by_patterns(frag, [_VOLUME_PVC_RE], pvcs, edges, self.weight)

    def _build_selector_edges(
        self,
        frag: Fragment,
        pods_with_labels: list[tuple[FragmentId, dict[str, str]]],
        edges: EdgeDict,
    ) -> None:
        kind, _, _ = _extract_resource_info(frag.content)
        if kind != "Service":
            return

        selector = self._get_service_selector(frag.content)
        if not selector:
            return

        for pod_id, labels in pods_with_labels:
            if pod_id != frag.id and _labels_match(selector, labels):
                self.add_edge(edges, frag.id, pod_id, self.selector_weight)

    def _get_service_selector(self, content: str) -> dict[str, str]:
        selector = _extract_selector_labels(content)
        if selector:
            return selector

        selector_match = _SIMPLE_SELECTOR_RE.search(content)
        if not selector_match:
            return {}

        return _extract_label_pairs(selector_match.group(1))

    def _build_image_edges(
        self,
        frag: Fragment,
        images: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for match in _IMAGE_RE.finditer(frag.content):
            image = match.group(1).strip()
            if not image or image.startswith("$"):
                continue
            for other_id in images.get(image, []):
                if other_id != frag.id:
                    self.add_edge(edges, frag.id, other_id, self.image_weight)
