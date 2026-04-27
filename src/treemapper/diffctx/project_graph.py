from __future__ import annotations

from pathlib import Path
from typing import Any

from _diffctx import build_project_graph as _rust_build_project_graph

ProjectGraph = Any  # opaque PyProjectGraph from the Rust extension


def build_project_graph(
    root_dir: Path,
    *,
    ignore_file: Path | None = None,
    no_default_ignores: bool = False,
    whitelist_file: Path | None = None,
) -> ProjectGraph:
    """Build a project graph by delegating to the Rust diffctx crate.

    The `ignore_file` / `no_default_ignores` / `whitelist_file` keyword
    arguments are accepted for API stability with the previous Python
    implementation; the Rust walker uses `git ls-files` and these inputs
    are currently a no-op. They will be honored once `universe.rs` exposes
    a path-spec layer.
    """
    del ignore_file, no_default_ignores, whitelist_file  # accepted, ignored for now
    return _rust_build_project_graph(str(root_dir))
