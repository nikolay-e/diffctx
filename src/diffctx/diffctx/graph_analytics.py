from __future__ import annotations

from typing import Any, cast

from diffctx._diffctx import (
    coupling_metrics as _rust_coupling_metrics,
)
from diffctx._diffctx import (
    detect_cycles as _rust_detect_cycles,
)
from diffctx._diffctx import (
    hotspots as _rust_hotspots,
)
from diffctx._diffctx import (
    quotient_graph as _rust_quotient_graph,
)
from diffctx._diffctx import (
    to_mermaid as _rust_to_mermaid,
)

QuotientGraph = Any  # opaque PyQuotientGraph from the Rust extension


def detect_cycles(
    pg: Any,
    level: str = "directory",
    edge_types: set[str] | None = None,
) -> list[list[str]]:
    types = sorted(edge_types) if edge_types else None
    return cast(list[list[str]], _rust_detect_cycles(pg, level, types))


def hotspots(
    pg: Any,
    top: int = 10,
    edge_types: set[str] | None = None,
) -> list[tuple[str, float, dict[str, int]]]:
    types = sorted(edge_types) if edge_types else None
    return cast(
        list[tuple[str, float, dict[str, int]]],
        _rust_hotspots(pg, top, types),
    )


def coupling_metrics(
    pg: Any,
    level: str = "directory",
    edge_types: set[str] | None = None,
) -> list[Any]:
    types = sorted(edge_types) if edge_types else None
    return cast(list[Any], _rust_coupling_metrics(pg, level, types))


def quotient_graph(pg: Any, level: str = "directory") -> QuotientGraph:
    return _rust_quotient_graph(pg, level)


def to_mermaid(qg: Any, top_n: int = 50) -> str:
    return cast(str, _rust_to_mermaid(qg, top_n))
