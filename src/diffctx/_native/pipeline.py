from __future__ import annotations

from pathlib import Path
from typing import Any

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
        )
    )


def resolve_diff_range(root_dir: Path, diff_range: str) -> str:
    from diffctx._diffctx import resolve_diff_range as _rust_resolve

    return str(_rust_resolve(str(root_dir), diff_range))


def get_raw_diff_text(root_dir: Path, diff_range: str, timeout: int = _PIPELINE_TIMEOUT) -> str:
    return _raw_diff_with_redactions(root_dir, diff_range, timeout, None)[0]


def _raw_diff_with_redactions(
    root_dir: Path, diff_range: str, timeout: int, paths: list[str] | None
) -> tuple[str, int, list[str]]:
    from diffctx._diffctx import get_raw_diff_text as _rust_raw_diff

    text, count, categories = _rust_raw_diff(str(root_dir), diff_range, timeout=timeout, paths=paths or [])
    return str(text), int(count), list(categories)


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
    effective_budget: int | None = _normalize_budget(budget_tokens)

    result: dict[str, Any] = _rust_build(
        str(root_dir),
        diff_range,
        budget_tokens=effective_budget,
        alpha=alpha,
        tau=tau,
        no_content=no_content,
        full=full,
        scoring_mode=scoring_mode,
        timeout=timeout,
        paths=paths or [],
    )

    # Attached after selection has already run, never before: the raw patch is
    # additive output and must not perturb the selected fragments.
    if with_raw_diff:
        raw_diff, redacted, categories = _raw_diff_with_redactions(root_dir, diff_range, timeout, paths)
        if redacted:
            _count_raw_diff_redactions(result, redacted, categories)
        if raw_diff:
            return _with_raw_diff_ahead_of_fragments(result, raw_diff)

    return result


# A secret on a removed line exists only in the patch; the document's
# redaction count and coverage block must say so like any other redaction.
def _count_raw_diff_redactions(result: dict[str, Any], count: int, categories: list[str]) -> None:
    block = dict(result.get("redactions") or {"count": 0, "categories": []})
    block["count"] = int(block.get("count", 0)) + count
    block["categories"] = [*block.get("categories", []), *(c for c in categories if c not in block.get("categories", []))]
    result["redactions"] = block
    empty_usage = dict.fromkeys(("source_bytes", "parsed_files", "candidate_files", "edge_contributions", "final_edges"), 0)
    coverage = dict(result.get("coverage") or {"status": "partial", "limit_reasons": [], "resources": empty_usage})
    reasons = list(coverage.get("limit_reasons") or [])
    if "sanitization_redaction" not in reasons:
        reasons.append("sanitization_redaction")
    coverage["limit_reasons"] = reasons
    result["coverage"] = coverage


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
