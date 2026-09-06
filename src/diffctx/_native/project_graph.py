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
    # Same footgun the diff pipeline closed: the graph backend applies no
    # custom --ignore/--whitelist file and no --no-default-ignores, so
    # accepting them silently let a caller excluding a secrets file believe
    # the exclusion took effect. Fail loudly until universe.rs exposes a
    # path-spec layer.
    if ignore_file is not None:
        raise NotImplementedError(
            "--ignore is not yet supported with --graph (default .gitignore/"
            ".diffctx/ignore rules still apply); rerun without --ignore"
        )
    if whitelist_file is not None:
        raise NotImplementedError("--whitelist is not yet supported with --graph; rerun without --whitelist")
    if no_default_ignores:
        raise NotImplementedError(
            "--no-default-ignores is not yet supported with --graph (default "
            "ignore rules still apply); rerun without --no-default-ignores"
        )
    return _rust_build_project_graph(str(root_dir))


def graph_root(pg: ProjectGraph) -> Path:
    return Path(pg.root_dir)
