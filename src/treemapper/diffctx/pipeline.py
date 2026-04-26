from __future__ import annotations

from pathlib import Path
from typing import Any


class DiffContextTimeoutError(Exception):
    pass


_PIPELINE_TIMEOUT = 300


_UNLIMITED_BUDGET = 10_000_000


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

    if budget_tokens is not None and budget_tokens < 0:
        effective_budget: int | None = _UNLIMITED_BUDGET
    elif budget_tokens == 0 or budget_tokens is None:
        effective_budget = None
    else:
        effective_budget = budget_tokens

    return _rust_build(  # type: ignore[no-any-return]
        str(root_dir),
        diff_range,
        budget_tokens=effective_budget,
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
