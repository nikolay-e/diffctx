from __future__ import annotations

import logging
from collections import defaultdict
from pathlib import Path

from .fragmentation import _create_whole_file_fragment
from .git import CatFileBatch
from .graph import Graph
from .select import SelectionResult, _IntervalIndex
from .types import Fragment, FragmentId

logger = logging.getLogger(__name__)


def _find_dangling_semantic_names(
    selected: list[Fragment],
    graph: Graph,
    frag_by_id: dict[FragmentId, Fragment],
    selected_ids: set[FragmentId],
) -> set[str]:
    dangling: set[str] = set()
    for frag in selected:
        for nbr_id in graph.neighbors(frag.id):
            if nbr_id in selected_ids:
                continue
            if graph.edge_categories.get((frag.id, nbr_id), "") != "semantic":
                continue
            nbr_frag = frag_by_id.get(nbr_id)
            if nbr_frag and nbr_frag.symbol_name:
                dangling.add(nbr_frag.symbol_name.lower())
    return dangling


def _pick_best_fragment(candidates: list[Fragment], selected_ids: set[FragmentId]) -> Fragment | None:
    if any(c.id in selected_ids for c in candidates):
        return None
    sig_candidates = [f for f in candidates if "_signature" in f.kind]
    full_candidates = [f for f in candidates if "_signature" not in f.kind]
    return next(iter(full_candidates or sig_candidates), None)


def _pick_smallest_fitting(
    candidates: list[Fragment],
    selected_ids: set[FragmentId],
    budget_left: int,
) -> Fragment | None:
    ranked = sorted(candidates, key=lambda f: f.token_count)
    for cand in ranked:
        if cand.token_count <= 0 or cand.id in selected_ids:
            continue
        if cand.token_count <= budget_left:
            return cand
    return None


def _coherence_post_pass(
    result: SelectionResult,
    all_fragments: list[Fragment],
    graph: Graph,
    budget: int,
) -> SelectionResult:
    selected_ids = {f.id for f in result.selected}
    interval_idx = _IntervalIndex()
    for f in result.selected:
        interval_idx.add(f.id)
    remaining = budget - result.used_tokens

    name_to_frags: dict[str, list[Fragment]] = {}
    for f in all_fragments:
        if f.symbol_name:
            name_to_frags.setdefault(f.symbol_name.lower(), []).append(f)

    frag_by_id: dict[FragmentId, Fragment] = {f.id: f for f in all_fragments}
    dangling_names = _find_dangling_semantic_names(result.selected, graph, frag_by_id, selected_ids)

    added: list[Fragment] = []
    for name in dangling_names:
        pick = _pick_best_fragment(name_to_frags.get(name, []), selected_ids)
        if pick and pick.token_count <= remaining and pick.id not in selected_ids and not interval_idx.overlaps(pick):
            added.append(pick)
            selected_ids.add(pick.id)
            interval_idx.add(pick.id)
            remaining -= pick.token_count

    if not added:
        return result

    new_selected = result.selected + added
    new_used = result.used_tokens + sum(f.token_count for f in added)
    new_utility = result.utility

    return SelectionResult(
        selected=new_selected,
        reason=result.reason,
        used_tokens=new_used,
        utility=new_utility,
    )


_RESCUE_BUDGET_FRACTION = 0.05
_RESCUE_MIN_SCORE_PERCENTILE = 0.80


def _compute_rescue_threshold(
    all_fragments: list[Fragment],
    rel_scores: dict[FragmentId, float],
    core_ids: set[FragmentId],
) -> float:
    context_scores = sorted(
        (rel_scores.get(f.id, 0.0) for f in all_fragments if f.id not in core_ids and rel_scores.get(f.id, 0.0) > 0),
        reverse=True,
    )
    if not context_scores:
        return float("inf")
    idx = int(len(context_scores) * (1 - _RESCUE_MIN_SCORE_PERCENTILE))
    return context_scores[min(idx, len(context_scores) - 1)]


def _collect_rescue_candidates(
    all_fragments: list[Fragment],
    rel_scores: dict[FragmentId, float],
    core_ids: set[FragmentId],
    selected_ids: set[FragmentId],
    selected_paths: set[Path],
    min_score: float,
    max_tokens: int,
) -> list[Fragment]:
    changed_paths = {fid.path for fid in core_ids}
    candidates = [
        f
        for f in all_fragments
        if f.id not in selected_ids
        and f.id not in core_ids
        and f.path not in changed_paths
        and f.path not in selected_paths
        and rel_scores.get(f.id, 0.0) >= min_score
        and f.token_count <= max_tokens
    ]
    candidates.sort(key=lambda f: rel_scores.get(f.id, 0.0), reverse=True)
    return candidates


def _rescue_nontrivial_context(
    result: SelectionResult,
    all_fragments: list[Fragment],
    rel_scores: dict[FragmentId, float],
    core_ids: set[FragmentId],
    budget: int,
) -> SelectionResult:
    remaining = budget - result.used_tokens
    rescue_budget = min(remaining, int(budget * _RESCUE_BUDGET_FRACTION))
    if rescue_budget <= 0:
        return result

    min_score = _compute_rescue_threshold(all_fragments, rel_scores, core_ids)
    if min_score == float("inf"):
        return result

    selected_ids = {f.id for f in result.selected}
    selected_paths = {f.path for f in result.selected}
    candidates = _collect_rescue_candidates(
        all_fragments, rel_scores, core_ids, selected_ids, selected_paths, min_score, rescue_budget
    )

    interval_idx = _IntervalIndex()
    for f in result.selected:
        interval_idx.add(f.id)

    added: list[Fragment] = []
    used = 0
    for cand in candidates:
        if used + cand.token_count > rescue_budget:
            continue
        if interval_idx.overlaps(cand):
            continue
        added.append(cand)
        interval_idx.add(cand.id)
        used += cand.token_count

    if not added:
        return result

    logger.debug("diffctx: rescued %d nontrivial context fragments (%d tokens)", len(added), used)
    return SelectionResult(
        selected=result.selected + added,
        reason=result.reason,
        used_tokens=result.used_tokens + used,
        utility=result.utility,
    )


def _ensure_changed_files_represented(
    selected: list[Fragment],
    all_fragments: list[Fragment],
    changed_files: list[Path],
    remaining_budget: int,
    root_dir: Path,
    preferred_revs: list[str],
    batch_reader: CatFileBatch | None = None,
) -> list[Fragment]:
    selected_paths = {f.path for f in selected}
    missing_paths = set(changed_files) - selected_paths

    if not missing_paths:
        return selected

    frags_by_path: dict[Path, list[Fragment]] = defaultdict(list)
    for f in all_fragments:
        if f.path in missing_paths:
            frags_by_path[f.path].append(f)

    added: list[Fragment] = []
    budget_left = remaining_budget
    selected_ids = {f.id for f in selected}
    interval_idx = _IntervalIndex()
    for f in selected:
        interval_idx.add(f.id)

    for path in sorted(missing_paths):
        candidates = frags_by_path.get(path, [])
        if not candidates:
            fallback = _create_whole_file_fragment(path, root_dir, preferred_revs, batch_reader)
            candidates = [fallback] if fallback else []

        picked = _pick_smallest_fitting(candidates, selected_ids, budget_left)
        if picked is not None and not interval_idx.overlaps(picked):
            added.append(picked)
            selected_ids.add(picked.id)
            interval_idx.add(picked.id)
            budget_left -= picked.token_count

    if added:
        logger.debug("diffctx: injected %d fragments to cover %d missing changed files", len(added), len(missing_paths))

    return selected + added
