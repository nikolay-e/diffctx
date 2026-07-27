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


# The Rust side drops the file sections diff mode never discloses (secret-like
# paths, ignored paths, lock files) so the bundled patch cannot widen what
# selection is willing to show. Nothing here feeds selection state (#150).
def get_raw_diff_text(root_dir: Path, diff_range: str, timeout: int = _PIPELINE_TIMEOUT) -> str:
    from diffctx._diffctx import get_raw_diff_text as _rust_raw_diff

    return str(_rust_raw_diff(str(root_dir), diff_range, timeout=timeout))


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
    with_raw_diff: bool = False,
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

    result: dict[str, Any] = _rust_build(
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

    # Attached after selection has already run, never before: the raw patch is
    # additive output and must not perturb the selected fragments.
    if with_raw_diff:
        raw_diff = get_raw_diff_text(root_dir, diff_range, timeout=timeout)
        if raw_diff:
            return _with_raw_diff_ahead_of_fragments(result, raw_diff)

    return result


# Readers consume the serialized output top-down, so the patch belongs above
# the fragments it explains — including in JSON, where key order is the only
# thing the writer preserves.
def _with_raw_diff_ahead_of_fragments(result: dict[str, Any], raw_diff: str) -> dict[str, Any]:
    ordered: dict[str, Any] = {}
    for key, value in result.items():
        if key in ("fragment_count", "fragments") and "raw_diff" not in ordered:
            ordered["raw_diff"] = raw_diff
        ordered[key] = value
    ordered.setdefault("raw_diff", raw_diff)
    return ordered
