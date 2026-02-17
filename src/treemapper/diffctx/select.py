from __future__ import annotations

import heapq
import logging
import statistics
from dataclasses import dataclass, field

from .types import Fragment, FragmentId
from .utility import InformationNeed, UtilityState, apply_fragment, compute_density, marginal_gain, utility_value

_BASELINE_K = 5


@dataclass
class SelectionResult:
    selected: list[Fragment]
    reason: str
    used_tokens: int
    utility: float


@dataclass
class _SelectionState:
    selected: list[Fragment] = field(default_factory=list)
    selected_ids: set[FragmentId] = field(default_factory=set)
    remaining_budget: int = 0
    utility_state: UtilityState = field(default_factory=UtilityState)


def _log_and_return(result: SelectionResult, core_ids: set[FragmentId]) -> SelectionResult:
    core_count = sum(1 for f in result.selected if f.id in core_ids)
    logging.debug(  # nosemgrep: python-logger-credential-disclosure
        "Selection: frags=%d core=%d tokens=%d reason=%s utility=%.4f",
        len(result.selected),
        core_count,
        result.used_tokens,
        result.reason,
        result.utility,
    )
    return result


def _select_core_fragments(
    core_fragments: list[Fragment],
    rel: dict[FragmentId, float],
    needs: tuple[InformationNeed, ...],
    state: _SelectionState,
) -> None:
    sorted_core = sorted(core_fragments, key=lambda f: rel.get(f.id, 0.0), reverse=True)

    for frag in sorted_core:
        if _is_subset_of_selected(frag, state.selected_ids):
            continue

        state.selected.append(frag)
        state.selected_ids.add(frag.id)
        state.remaining_budget -= frag.token_count
        apply_fragment(frag, rel.get(frag.id, 0.0), needs, state.utility_state)


@dataclass
class _HeapEntry:
    neg_density: float
    frag_id: FragmentId
    version: int

    def __lt__(self, other: _HeapEntry) -> bool:
        return self.neg_density < other.neg_density


def _build_initial_heap(
    candidates: list[Fragment],
    rel: dict[FragmentId, float],
    needs: tuple[InformationNeed, ...],
    state: UtilityState,
    id_to_frag: dict[FragmentId, Fragment],
) -> list[_HeapEntry]:
    heap: list[_HeapEntry] = []
    for frag in candidates:
        if frag.token_count > 0:
            density = compute_density(frag, rel.get(frag.id, 0.0), needs, state)
            heapq.heappush(heap, _HeapEntry(-density, frag.id, 0))
            id_to_frag[frag.id] = frag
    return heap


def _find_best_candidate_heap(
    heap: list[_HeapEntry],
    current_version: int,
    id_to_frag: dict[FragmentId, Fragment],
    selected_ids: set[FragmentId],
    remaining_budget: int,
    rel: dict[FragmentId, float],
    needs: tuple[InformationNeed, ...],
    state: UtilityState,
) -> tuple[Fragment | None, float, int]:
    while heap:
        entry = heapq.heappop(heap)
        frag = id_to_frag.get(entry.frag_id)

        if frag is None:
            continue
        if frag.token_count > remaining_budget:
            continue
        if _overlaps_with_selected(frag, selected_ids):
            continue

        if entry.version < current_version:
            new_density = compute_density(frag, rel.get(frag.id, 0.0), needs, state)
            heapq.heappush(heap, _HeapEntry(-new_density, frag.id, current_version))
            continue

        actual_density = -entry.neg_density
        if actual_density <= 0:
            return None, 0.0, current_version

        return frag, actual_density, current_version + 1

    return None, 0.0, current_version


def _find_best_singleton(
    non_core: list[Fragment],
    base_selected_ids: set[FragmentId],
    base_budget: int,
    rel: dict[FragmentId, float],
    needs: tuple[InformationNeed, ...],
    base_state: UtilityState,
) -> tuple[Fragment | None, float]:
    best_singleton = None
    best_gain = 0.0
    for f in non_core:
        if f.token_count > base_budget:
            continue
        if _overlaps_with_selected(f, base_selected_ids):
            continue
        gain = marginal_gain(f, rel.get(f.id, 0.0), needs, base_state)
        if gain > best_gain:
            best_gain = gain
            best_singleton = f
    return best_singleton, best_gain


def lazy_greedy_select(
    fragments: list[Fragment],
    core_ids: set[FragmentId],
    rel: dict[FragmentId, float],
    needs: tuple[InformationNeed, ...],
    budget_tokens: int,
    tau: float = 0.08,
) -> SelectionResult:
    if not fragments:
        return _log_and_return(
            SelectionResult(selected=[], reason="no_candidates", used_tokens=0, utility=0.0),
            core_ids,
        )

    core_fragments = [f for f in fragments if f.id in core_ids]
    core_fragments.sort(key=lambda f: (f.token_count if f.token_count > 0 else 10**9, f.line_count, f.start_line))
    non_core_fragments = [f for f in fragments if f.id not in core_ids]

    state = _SelectionState(remaining_budget=budget_tokens)
    _select_core_fragments(core_fragments, rel, needs, state)

    if state.remaining_budget <= 0:
        used = budget_tokens - state.remaining_budget
        return _log_and_return(
            SelectionResult(
                selected=state.selected,
                reason="budget_exhausted",
                used_tokens=used,
                utility=utility_value(state.utility_state),
            ),
            core_ids,
        )

    base_state = state.utility_state.copy()
    base_selected = list(state.selected)
    base_budget = state.remaining_budget

    candidates = [f for f in non_core_fragments if not _overlaps_with_selected(f, state.selected_ids)]
    id_to_frag: dict[FragmentId, Fragment] = {}
    heap = _build_initial_heap(candidates, rel, needs, state.utility_state, id_to_frag)

    selections_for_baseline, threshold = _run_greedy_loop_heap(heap, id_to_frag, state, rel, needs, tau)

    greedy_utility = utility_value(state.utility_state)
    base_selected_ids = {f.id for f in base_selected}

    singleton_result = _try_singleton_improvement(
        non_core_fragments,
        base_selected_ids,
        base_budget,
        base_selected,
        rel,
        needs,
        base_state,
        greedy_utility,
        budget_tokens,
        core_ids,
    )
    if singleton_result is not None:
        return singleton_result

    return _determine_final_result(
        state, base_selected, budget_tokens, greedy_utility, selections_for_baseline, threshold, bool(heap), core_ids
    )


def _run_greedy_loop_heap(
    heap: list[_HeapEntry],
    id_to_frag: dict[FragmentId, Fragment],
    state: _SelectionState,
    rel: dict[FragmentId, float],
    needs: tuple[InformationNeed, ...],
    tau: float,
) -> tuple[int, float]:
    baseline_densities: list[float] = []
    threshold = 0.0
    selections_for_baseline = 0
    current_version = 0

    while heap and state.remaining_budget > 0:
        best_frag, best_density, current_version = _find_best_candidate_heap(
            heap,
            current_version,
            id_to_frag,
            state.selected_ids,
            state.remaining_budget,
            rel,
            needs,
            state.utility_state,
        )

        if best_frag is None or best_density <= 0:
            break

        if selections_for_baseline < _BASELINE_K:
            baseline_densities.append(best_density)
            selections_for_baseline += 1
            if selections_for_baseline == _BASELINE_K and baseline_densities:
                threshold = tau * statistics.median(baseline_densities)
        elif best_density < threshold:
            break

        state.selected.append(best_frag)
        state.selected_ids.add(best_frag.id)
        state.remaining_budget -= best_frag.token_count
        apply_fragment(best_frag, rel.get(best_frag.id, 0.0), needs, state.utility_state)

    return selections_for_baseline, threshold


def _try_singleton_improvement(
    non_core: list[Fragment],
    base_selected_ids: set[FragmentId],
    base_budget: int,
    base_selected: list[Fragment],
    rel: dict[FragmentId, float],
    needs: tuple[InformationNeed, ...],
    base_state: UtilityState,
    greedy_utility: float,
    budget_tokens: int,
    core_ids: set[FragmentId],
) -> SelectionResult | None:
    best_singleton, best_gain = _find_best_singleton(non_core, base_selected_ids, base_budget, rel, needs, base_state)

    if best_singleton is None:
        return None

    singleton_utility = utility_value(base_state) + best_gain
    if singleton_utility <= greedy_utility:
        return None

    used = budget_tokens - (base_budget - best_singleton.token_count)
    return _log_and_return(
        SelectionResult(
            selected=[*base_selected, best_singleton],
            reason="best_singleton",
            used_tokens=used,
            utility=singleton_utility,
        ),
        core_ids,
    )


def _determine_final_result(
    state: _SelectionState,
    base_selected: list[Fragment],
    budget_tokens: int,
    greedy_utility: float,
    selections_for_baseline: int,
    threshold: float,
    has_remaining_candidates: bool,
    core_ids: set[FragmentId],
) -> SelectionResult:
    used = budget_tokens - state.remaining_budget

    if state.remaining_budget <= 0:
        reason = "budget_exhausted"
    elif greedy_utility <= 0:
        reason = "no_utility"
    elif not state.selected or len(state.selected) == len(base_selected):
        reason = "no_candidates"
    elif selections_for_baseline >= _BASELINE_K and threshold > 0 and has_remaining_candidates:
        reason = "stopped_by_tau"
    else:
        reason = "no_candidates"

    return _log_and_return(
        SelectionResult(selected=state.selected, reason=reason, used_tokens=used, utility=greedy_utility),
        core_ids,
    )


def _overlaps_with_selected(frag: Fragment, selected_ids: set[FragmentId]) -> bool:
    for sel_id in selected_ids:
        if sel_id.path != frag.path:
            continue
        if sel_id == frag.id:
            continue
        if max(frag.start_line, sel_id.start_line) <= min(frag.end_line, sel_id.end_line):
            return True
    return False


def _is_subset_of_selected(frag: Fragment, selected_ids: set[FragmentId]) -> bool:
    for sel_id in selected_ids:
        if sel_id.path != frag.path:
            continue
        if sel_id == frag.id:
            continue
        if sel_id.start_line <= frag.start_line and frag.end_line <= sel_id.end_line:
            return True
    return False
