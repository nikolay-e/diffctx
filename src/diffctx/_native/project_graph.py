from __future__ import annotations

from pathlib import Path
from typing import Any

from diffctx._diffctx import build_project_graph as _rust_build_project_graph

ProjectGraph = Any  # opaque PyProjectGraph from the Rust extension


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
    return _rust_build_project_graph(str(root_dir))


def graph_root(pg: ProjectGraph) -> Path:
    return Path(pg.root_dir)
