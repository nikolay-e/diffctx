from __future__ import annotations

from pathlib import Path
from typing import Any, NamedTuple

# Mirrors the pyo3 signature defaults by reading them, so this wrapper cannot
# drift from the engine it wraps. Python always passes `scoring_mode`
# explicitly, so a literal here would quietly override a changed engine default
# for every Python and MCP caller.
#
# `tau` is the exception and travels as `None` when unspecified: the engine
# resolves an unnamed threshold differently per scorer, and a wrapper that
# substitutes the default here takes that choice away from it.
from diffctx._diffctx import DEFAULT_ALPHA as _DEFAULT_ALPHA
from diffctx._diffctx import DEFAULT_SCORING as _DEFAULT_SCORING
from diffctx._diffctx import DEFAULT_TIMEOUT as _PIPELINE_TIMEOUT

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
    alpha: float = _DEFAULT_ALPHA,
    scoring_mode: str = _DEFAULT_SCORING,
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
    tau: float | None = None,
    no_content: bool = False,
) -> dict[str, Any]:
    """Light-phase select+postpass+render against a precomputed state."""
    from diffctx._diffctx import select_with_params as _rust_select

    return _rust_select(
        state,
        budget_tokens=_normalize_budget(budget_tokens),
        tau=tau,
        no_content=no_content,
    )


# The Rust side drops the file sections diff mode never discloses (secret-like
# paths, ignored paths, lock files) so the bundled patch cannot widen what
# selection is willing to show. Nothing here feeds selection state (#150).
def build_locate(
    root_dir: Path,
    diff_range: str,
    budget_tokens: int | None = None,
    alpha: float = _DEFAULT_ALPHA,
    tau: float | None = None,
    scoring_mode: str = _DEFAULT_SCORING,
    timeout: int = _PIPELINE_TIMEOUT,
    paths: list[str] | None = None,
    include_overflow: bool = False,
) -> str:
    from diffctx._diffctx import build_locate as _rust_locate

    return str(
        _rust_locate(
            str(root_dir),
            diff_range,
            budget_tokens=_normalize_budget(budget_tokens),
            alpha=alpha,
            tau=tau,
            scoring_mode=scoring_mode,
            timeout=timeout,
            paths=paths or [],
            include_overflow=include_overflow,
        )
    )


def build_impact(
    root_dir: Path,
    diff_range: str,
    alpha: float = _DEFAULT_ALPHA,
    scoring_mode: str = _DEFAULT_SCORING,
    timeout: int = _PIPELINE_TIMEOUT,
    paths: list[str] | None = None,
    markdown: bool = False,
    symbol: str | None = None,
    budget: int | None = None,
    unfold: bool = False,
) -> str:
    from diffctx._diffctx import build_impact as _rust_impact

    return str(
        _rust_impact(
            str(root_dir),
            diff_range,
            alpha=alpha,
            scoring_mode=scoring_mode,
            timeout=timeout,
            paths=paths or [],
            markdown=markdown,
            symbol=symbol,
            budget=budget,
            unfold=unfold,
        )
    )


def get_raw_diff_text(root_dir: Path, diff_range: str, timeout: int = _PIPELINE_TIMEOUT) -> str:
    from diffctx._diffctx import get_raw_diff_text as _rust_raw_diff

    text, _, _ = _rust_raw_diff(str(root_dir), diff_range, timeout=timeout, paths=[])
    return str(text)


def build_diff_context(
    root_dir: Path,
    diff_range: str,
    budget_tokens: int | None = None,
    alpha: float = _DEFAULT_ALPHA,
    tau: float | None = None,
    no_content: bool = False,
    full: bool = False,
    scoring_mode: str = _DEFAULT_SCORING,
    timeout: int = _PIPELINE_TIMEOUT,
    with_raw_diff: bool = False,
    paths: list[str] | None = None,
) -> dict[str, Any]:
    from diffctx._diffctx import build_diff_context as _rust_build

    # Budget semantics:
    #   None:                   pipeline default (None passes through to Rust as no cap)
    #   budget_tokens < 0:      "unlimited" (10M-token soft ceiling, used as the recall ceiling
    #                            sanity bound in evaluation matrices)
    #   budget_tokens == 0:     no fragments at all (the change summary alone; recall floor)
    #   budget_tokens > 0:      explicit cap
    # The raw patch is attached after selection has run, ahead of the fragments
    # it explains, and its redactions are counted: all in the engine.
    result: dict[str, Any] = _rust_build(
        str(root_dir),
        diff_range,
        budget_tokens=_normalize_budget(budget_tokens),
        alpha=alpha,
        tau=tau,
        no_content=no_content,
        full=full,
        scoring_mode=scoring_mode,
        timeout=timeout,
        paths=paths or [],
        with_raw_diff=with_raw_diff,
    )
    return result


class Delivery(NamedTuple):
    """What a door prints and exits with: decided by the engine, never by the door."""

    text: str
    notes: list[tuple[str, str]]
    exit: int
    listed: int
    latency: dict[str, Any] | None


def deliver(
    root_dir: Path,
    mode: str,
    diff_range: str = "",
    symbol: str | None = None,
    paths: list[str] | None = None,
    budget: int | None = None,
    ceiling: int | None = None,
    envelope: str = "",
    format: str = "md",
    unfold: bool = False,
    alpha: float = _DEFAULT_ALPHA,
    tau: float | None = None,
    scoring_mode: str = _DEFAULT_SCORING,
    timeout: int = _PIPELINE_TIMEOUT,
    no_content: bool = False,
    full: bool = False,
    with_raw_diff: bool = False,
    overflow: bool = False,
) -> Delivery:
    """One engine call for a diff question: the reply text, the stderr notes as
    (kind, text) pairs (`warning` always said; `detail` and `figure` unless quiet),
    the exit status, how many fragments or items the text lists, and telemetry.
    `ceiling` is a hard cap on the reply, `envelope` included: a budget above it
    is fitted to it, never refused."""
    from diffctx._diffctx import deliver as _rust_deliver

    return Delivery(
        *_rust_deliver(
            str(root_dir),
            mode,
            diff_range,
            symbol=symbol,
            paths=paths or [],
            budget=budget,
            ceiling=ceiling,
            envelope=envelope,
            format=format,
            unfold=unfold,
            alpha=alpha,
            tau=tau,
            scoring_mode=scoring_mode,
            timeout=timeout,
            no_content=no_content,
            full=full,
            with_raw_diff=with_raw_diff,
            overflow=overflow,
        )
    )
