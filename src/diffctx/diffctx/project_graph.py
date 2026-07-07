from __future__ import annotations

from pathlib import Path
from typing import Any

from diffctx._diffctx import build_project_graph as _rust_build_project_graph

ProjectGraph = Any  # opaque PyProjectGraph from the Rust extension

_GRAPH_ROOTS: dict[int, Path] = {}
_GRAPH_ROOTS_MAX = 16


def build_project_graph(
    root_dir: Path,
    *,
    ignore_file: Path | None = None,
    no_default_ignores: bool = False,
    whitelist_file: Path | None = None,
) -> ProjectGraph:
    # ignore_file / no_default_ignores / whitelist_file are accepted for API
    # stability but are a no-op until universe.rs exposes a path-spec layer.
    del ignore_file, no_default_ignores, whitelist_file
    pg = _rust_build_project_graph(str(root_dir))
    _register_graph_root(pg, Path(root_dir).resolve())
    return pg


def _register_graph_root(pg: ProjectGraph, root: Path) -> None:
    if len(_GRAPH_ROOTS) >= _GRAPH_ROOTS_MAX:
        _GRAPH_ROOTS.clear()
    _GRAPH_ROOTS[id(pg)] = root


def graph_root(pg: ProjectGraph) -> Path | None:
    return _GRAPH_ROOTS.get(id(pg))
