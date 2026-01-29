from __future__ import annotations

from .build import BuildSystemEdgeBuilder
from .cicd import CICDEdgeBuilder
from .docker import DockerEdgeBuilder
from .generic import ConfigToCodeEdgeBuilder
from .helm import HelmEdgeBuilder
from .kubernetes import KubernetesEdgeBuilder
from .terraform import TerraformEdgeBuilder


def get_config_builders() -> list[type]:
    return [
        DockerEdgeBuilder,
        TerraformEdgeBuilder,
        HelmEdgeBuilder,
        KubernetesEdgeBuilder,
        CICDEdgeBuilder,
        BuildSystemEdgeBuilder,
        ConfigToCodeEdgeBuilder,
    ]


__all__ = [
    "BuildSystemEdgeBuilder",
    "CICDEdgeBuilder",
    "ConfigToCodeEdgeBuilder",
    "DockerEdgeBuilder",
    "HelmEdgeBuilder",
    "KubernetesEdgeBuilder",
    "TerraformEdgeBuilder",
    "get_config_builders",
]
