from __future__ import annotations

from pathlib import Path
from typing import Any

_PIPELINE_TIMEOUT = 300


_UNLIMITED_BUDGET = 10_000_000


def _normalize_budget(budget_tokens: int | None) -> int | None:
    if budget_tokens is None:
        return None
    if budget_tokens < 0:
        return _UNLIMITED_BUDGET
    return budget_tokens


def compute_scored_state(
    root_dir: Path,
    diff_range: str,
    alpha: float = 0.60,
    scoring_mode: str = "ego",
    timeout: int = _PIPELINE_TIMEOUT,
) -> Any:
    """Heavy-phase compute, returns an opaque PyScoredState. Reuse it
    across many `select_with_params` calls to sweep a (tau, cbf) grid
    without re-doing parse/fragment/discover/score work."""
    from diffctx._diffctx import compute_scored_state as _rust_compute

    return _rust_compute(
        str(root_dir),
        diff_range,
        alpha=alpha,
        scoring_mode=scoring_mode,
        timeout=timeout,
    )


def select_with_params(
    state: Any,
    budget_tokens: int | None = None,
    tau: float = 0.12,
    no_content: bool = False,
) -> dict[str, Any]:
    """Light-phase select+postpass+render against a precomputed state."""
    from diffctx._diffctx import select_with_params as _rust_select

    return _rust_select(  # type: ignore[no-any-return]
        state,
        budget_tokens=_normalize_budget(budget_tokens),
        tau=tau,
        no_content=no_content,
    )


def build_diff_context(
    root_dir: Path,
    diff_range: str,
    budget_tokens: int | None = None,
    alpha: float = 0.60,
    tau: float = 0.12,
    no_content: bool = False,
    ignore_file: Path | None = None,
    no_default_ignores: bool = False,
    full: bool = False,
    whitelist_file: Path | None = None,
    scoring_mode: str = "ego",
    timeout: int = _PIPELINE_TIMEOUT,
) -> dict[str, Any]:
    from diffctx._diffctx import build_diff_context as _rust_build

    # The Rust diff-context backend does not yet apply a custom --ignore/
    # --whitelist file (default .gitignore/.diffctx/ignore rules ARE applied).
    # Silently accepting and dropping these was a security-adjacent footgun -
    # a caller excluding a secrets file via -i would get no warning that the
    # exclusion never took effect. Fail loudly instead until implemented.
    if ignore_file is not None:
        raise NotImplementedError(
            "--ignore is not yet supported with --diff (default .gitignore/"
            ".diffctx/ignore rules still apply); rerun without --ignore, or "
            "without --diff to use it in tree-mapping mode"
        )
    if whitelist_file is not None:
        raise NotImplementedError(
            "--whitelist is not yet supported with --diff; rerun without "
            "--whitelist, or without --diff to use it in tree-mapping mode"
        )

    # Budget semantics:
    #   None:                   pipeline default (None passes through to Rust as no cap)
    #   budget_tokens < 0:      "unlimited" (10M-token soft ceiling, used as the recall ceiling
    #                            sanity bound in evaluation matrices)
    #   budget_tokens == 0:     "no context" (recall floor; only the diff itself, no expansion)
    #   budget_tokens > 0:      explicit cap
    effective_budget: int | None = _normalize_budget(budget_tokens)

    return _rust_build(  # type: ignore[no-any-return]
        str(root_dir),
        diff_range,
        budget_tokens=effective_budget,
        alpha=alpha,
        tau=tau,
        no_content=no_content,
        # Both are guaranteed None here: the guards above raise on any
        # other value until the Rust backend implements them.
        ignore_file=None,
        no_default_ignores=no_default_ignores,
        full=full,
        whitelist_file=None,
        scoring_mode=scoring_mode,
        timeout=timeout,
    )
