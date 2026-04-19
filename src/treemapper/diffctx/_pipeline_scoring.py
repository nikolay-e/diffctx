from __future__ import annotations

import logging
import os
from pathlib import Path
from typing import Any

from ..tokens import count_tokens
from .config import LIMITS
from .file_importance import compute_file_importance
from .postpass import _coherence_post_pass, _rescue_nontrivial_context
from .scoring import PPRScoring, ScoringStrategy
from .select import lazy_greedy_select
from .types import Fragment, FragmentId
from .utility import needs_from_diff

logger = logging.getLogger(__name__)

_OVERHEAD_PER_FRAGMENT = LIMITS.overhead_per_fragment
_UNLIMITED_BUDGET = 10_000_000


def _count_one(frag: Fragment) -> tuple[Fragment, int]:
    return frag, count_tokens(frag.content).count + _OVERHEAD_PER_FRAGMENT


def assign_token_counts(fragments: list[Fragment]) -> None:
    if len(fragments) < 20:
        for frag in fragments:
            frag.token_count = count_tokens(frag.content).count + _OVERHEAD_PER_FRAGMENT
        return
    from concurrent.futures import ThreadPoolExecutor

    with ThreadPoolExecutor(max_workers=2) as pool:
        for frag, tc in pool.map(_count_one, fragments):
            frag.token_count = tc


def select_full_mode(
    all_fragments: list[Fragment],
    changed_files: list[Path],
) -> list[Fragment]:
    changed_paths = set(changed_files)
    selected = [f for f in all_fragments if f.path in changed_paths]
    selected.sort(key=lambda f: (f.path, f.start_line))
    return selected


def score_and_select(
    all_fragments: list[Fragment],
    core_ids: set[FragmentId],
    diff_text: str,
    budget_tokens: int | None,
    tau: float,
    hunks: list[Any],
    repo_root: Path | None = None,
    seed_weights: dict[FragmentId, float] | None = None,
    scoring_strategy: ScoringStrategy | None = None,
    discovered_paths: set[Path] | None = None,
) -> tuple[list[Fragment], Any]:
    strategy = scoring_strategy or PPRScoring()

    dump_scores = os.environ.get("DIFFCTX_DUMP_SCORES")
    scoring_result = strategy.score_and_filter(
        all_fragments,
        core_ids,
        hunks,
        repo_root=repo_root,
        seed_weights=seed_weights,
        dump_scores_file=dump_scores,
        discovered_paths=discovered_paths,
    )

    needs = needs_from_diff(scoring_result.filtered_fragments, core_ids, scoring_result.graph, diff_text)
    file_importance = compute_file_importance(scoring_result.filtered_fragments)
    effective_budget = budget_tokens if budget_tokens is not None else _UNLIMITED_BUDGET

    if os.environ.get("DIFFCTX_NO_SUBMODULAR"):
        from .select import _topk_select

        result = _topk_select(scoring_result.filtered_fragments, core_ids, scoring_result.rel_scores, effective_budget)
    else:
        result = lazy_greedy_select(
            fragments=scoring_result.filtered_fragments,
            core_ids=core_ids,
            rel=scoring_result.rel_scores,
            needs=needs,
            budget_tokens=effective_budget,
            tau=tau,
            file_importance=file_importance,
        )

    selected = _coherence_post_pass(result, scoring_result.filtered_fragments, scoring_result.graph, effective_budget)
    selected = _rescue_nontrivial_context(selected, all_fragments, scoring_result.rel_scores, core_ids, effective_budget)
    return selected.selected, selected


def log_full_mode(selected: list[Fragment]) -> None:
    used = sum(f.token_count for f in selected)
    logger.info(
        "diffctx: full mode selected=%d from changed files used=%d tokens",
        len(selected),
        used,
    )


def log_ppr_mode(
    selected: list[Fragment],
    core_ids: set[FragmentId],
    budget_tokens: int | None,
    result: Any,
    alpha: float,
    tau: float,
) -> None:
    used = sum(f.token_count for f in selected)
    budget_str = str(budget_tokens) if budget_tokens is not None else "unlimited"
    logger.info(
        "diffctx: selected=%d core=%d used=%d/%s reason=%s utility=%.4f alpha=%.3f tau=%.3f",
        len(selected),
        len(core_ids),
        used,
        budget_str,
        result.reason,
        result.utility,
        alpha,
        tau,
    )
