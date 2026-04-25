from __future__ import annotations

from pathlib import Path
from typing import Any


class DiffContextTimeoutError(Exception):
    pass


_PIPELINE_TIMEOUT = 300


def build_diff_context(
    root_dir: Path,
    diff_range: str,
    budget_tokens: int | None = None,
    alpha: float = 0.60,
    tau: float = 0.08,
    no_content: bool = False,
    ignore_file: Path | None = None,
    no_default_ignores: bool = False,
    full: bool = False,
    whitelist_file: Path | None = None,
    scoring_mode: str = "hybrid",
    timeout: int = _PIPELINE_TIMEOUT,
) -> dict[str, Any]:
    from _diffctx import build_diff_context as _rust_build

    return _rust_build(  # type: ignore[no-any-return]
        str(root_dir),
        diff_range,
        budget_tokens=budget_tokens,
        alpha=alpha,
        tau=tau,
        no_content=no_content,
        ignore_file=str(ignore_file) if ignore_file else None,
        no_default_ignores=no_default_ignores,
        full=full,
        whitelist_file=str(whitelist_file) if whitelist_file else None,
        scoring_mode=scoring_mode,
        timeout=timeout,
    )
