use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::limits::UTILITY;
use crate::config::selection::selection;
use crate::interval::IntervalIndex;
use crate::types::{Fragment, FragmentId};
use crate::utility::needs::InformationNeed;
use crate::utility::scoring::{
    UtilityState, apply_fragment, compute_density, marginal_gain, utility_value,
};

const SENTINEL_TOKEN_COUNT: u32 = 1_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionReason {
    TopK,
    NoCandidates,
    BudgetExhausted,
    NoUtility,
    StoppedByTau,
    BestSingleton,
}

impl SelectionReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TopK => "topk",
            Self::NoCandidates => "no_candidates",
            Self::BudgetExhausted => "budget_exhausted",
            Self::NoUtility => "no_utility",
            Self::StoppedByTau => "stopped_by_tau",
            Self::BestSingleton => "best_singleton",
        }
    }
}

pub struct SelectionResult {
    pub selected: Vec<Fragment>,
    pub reason: SelectionReason,
    pub used_tokens: u32,
    pub utility: f64,
    /// Greedy iterations actually executed (number of `apply_fragment`
    /// calls in `run_greedy_loop_heap`). Diagnoses lazy-heap blowup:
    /// expected ≈ output size, pathological ≫ output size when
    /// stale-version rejections dominate.
    pub greedy_iters: usize,
}

struct HeapEntry {
    neg_density: f64,
    frag_id: FragmentId,
    version: u32,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.neg_density.to_bits() == other.neg_density.to_bits() && self.frag_id == other.frag_id
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other.neg_density.total_cmp(&self.neg_density)
    }
}

struct SelectionState {
    selected: Vec<Fragment>,
    selected_ids: IntervalIndex,
    remaining_budget: u32,
    utility_state: UtilityState,
}

fn drop_redundant_signatures(candidates: &[Fragment], budget: u32) -> Vec<Fragment> {
    let mut full_token_by_loc: FxHashMap<(Arc<str>, u32), u32> = FxHashMap::default();
    for f in candidates {
        if !f.kind.is_signature() {
            full_token_by_loc.insert((f.id.path.clone(), f.start_line()), f.token_count);
        }
    }
    candidates
        .iter()
        .filter(|f| {
            if !f.kind.is_signature() {
                return true;
            }
            let key = (f.id.path.clone(), f.start_line());
            full_token_by_loc
                .get(&key)
                .copied()
                .unwrap_or(SENTINEL_TOKEN_COUNT)
                > budget
        })
        .cloned()
        .collect()
}

fn compute_r_cap(
    rel: &FxHashMap<FragmentId, f64>,
    core_ids: Option<&FxHashSet<FragmentId>>,
) -> f64 {
    let values: Vec<f64> = rel
        .iter()
        .filter(|(fid, v)| **v > 0.0 && core_ids.map_or(true, |c| !c.contains(*fid)))
        .map(|(_, v)| *v)
        .collect();

    if values.len() < 2 {
        return if let Some(&v) = values.first() {
            v.max(selection().r_cap_min)
        } else {
            1.0
        };
    }

    let mut sorted = values.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let mid = sorted.len() / 2;
    let med = if sorted.len() % 2 == 0 {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    };

    let mean: f64 = values.iter().sum::<f64>() / values.len() as f64;
    let variance: f64 =
        values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (values.len() - 1) as f64;
    let std = variance.sqrt();

    (med + UTILITY.r_cap_sigma * std).max(1e-9)
}

fn build_signature_lookup(
    fragments: &[Fragment],
    core_fragments: &[Fragment],
) -> FxHashMap<FragmentId, Fragment> {
    let mut sig_by_loc: FxHashMap<(Arc<str>, u32), Fragment> = FxHashMap::default();
    for f in fragments {
        if f.kind.is_signature() {
            sig_by_loc.insert((f.id.path.clone(), f.start_line()), f.clone());
        }
    }
    let mut sig_lookup = FxHashMap::default();
    for cf in core_fragments {
        let key = (cf.id.path.clone(), cf.start_line());
        if let Some(sig) = sig_by_loc.get(&key) {
            sig_lookup.insert(cf.id.clone(), sig.clone());
        }
    }
    sig_lookup
}

fn select_core_fragments(
    core_fragments: &[Fragment],
    rel: &FxHashMap<FragmentId, f64>,
    needs: &[InformationNeed],
    state: &mut SelectionState,
    budget_tokens: u32,
    sig_lookup: &FxHashMap<FragmentId, Fragment>,
) {
    let core_budget = (budget_tokens as f64 * selection().core_budget_fraction) as u32;
    // Counter for cores placed; the first pass keeps `core_used <= core_budget`,
    // but the rescue pass below intentionally allows it to exceed `core_budget`
    // up to `budget_tokens`. Don't assume the tighter bound past this scope.
    let mut core_used = 0u32;

    let mut sorted_core: Vec<&Fragment> = core_fragments.iter().collect();
    sorted_core.sort_by(|a, b| {
        let ra = rel.get(&a.id).copied().unwrap_or(0.0);
        let rb = rel.get(&b.id).copied().unwrap_or(0.0);
        rb.total_cmp(&ra)
    });

    let place_fragment =
        |frag: &Fragment, core_used: &mut u32, state: &mut SelectionState, rel_score: f64| {
            state.selected.push(frag.clone());
            state.selected_ids.add_id(&frag.id);
            state.remaining_budget = state.remaining_budget.saturating_sub(frag.token_count);
            *core_used += frag.token_count;
            apply_fragment(frag, rel_score, needs, &mut state.utility_state);
        };

    let mut skipped: Vec<&Fragment> = Vec::new();
    for frag in &sorted_core {
        if state.selected_ids.is_superset_of(frag) {
            continue;
        }
        if core_used + frag.token_count > core_budget {
            if let Some(sig) = sig_lookup.get(&frag.id) {
                if !state.selected_ids.contains(&sig.id)
                    && core_used + sig.token_count <= core_budget
                {
                    let rel_score = rel.get(&frag.id).copied().unwrap_or(0.0);
                    place_fragment(sig, &mut core_used, state, rel_score);
                    continue;
                }
            }
            skipped.push(frag);
            continue;
        }

        let rel_score = rel.get(&frag.id).copied().unwrap_or(0.0);
        place_fragment(frag, &mut core_used, state, rel_score);
    }

    // Bug #2 fix: cores that didn't fit the core_budget reservation must not be
    // demoted to ordinary greedy candidates without a chance to be placed first.
    // Sweep skipped cores cheapest-first against the *full* remaining budget
    // (not just the core slice) so seeds aren't dropped purely because the
    // highest-relevance core happened to be heavy.
    if !skipped.is_empty() {
        skipped.sort_by(|a, b| a.token_count.cmp(&b.token_count));
        for frag in skipped {
            if state.remaining_budget == 0 {
                break;
            }
            if state.selected_ids.is_superset_of(frag) {
                continue;
            }
            if frag.token_count <= state.remaining_budget {
                let rel_score = rel.get(&frag.id).copied().unwrap_or(0.0);
                place_fragment(frag, &mut core_used, state, rel_score);
            } else if let Some(sig) = sig_lookup.get(&frag.id) {
                if !state.selected_ids.contains(&sig.id)
                    && sig.token_count <= state.remaining_budget
                {
                    let rel_score = rel.get(&frag.id).copied().unwrap_or(0.0);
                    place_fragment(sig, &mut core_used, state, rel_score);
                }
            }
        }
    }
}

fn build_initial_heap(
    candidates: &[Fragment],
    rel: &FxHashMap<FragmentId, f64>,
    needs: &[InformationNeed],
    state: &UtilityState,
    id_to_frag: &mut FxHashMap<FragmentId, Fragment>,
) -> BinaryHeap<HeapEntry> {
    let mut heap = BinaryHeap::new();
    for frag in candidates {
        if frag.token_count > 0 {
            let density = compute_density(
                frag,
                rel.get(&frag.id).copied().unwrap_or(0.0),
                needs,
                state,
            );
            heap.push(HeapEntry {
                neg_density: -density,
                frag_id: frag.id.clone(),
                version: 0,
            });
            id_to_frag.insert(frag.id.clone(), frag.clone());
        }
    }
    heap
}

fn find_best_candidate_heap(
    heap: &mut BinaryHeap<HeapEntry>,
    current_version: u32,
    id_to_frag: &FxHashMap<FragmentId, Fragment>,
    selected_ids: &IntervalIndex,
    remaining_budget: u32,
    rel: &FxHashMap<FragmentId, f64>,
    needs: &[InformationNeed],
    state: &UtilityState,
) -> (Option<Fragment>, f64, u32) {
    let cv = current_version;
    while let Some(entry) = heap.pop() {
        let frag = match id_to_frag.get(&entry.frag_id) {
            Some(f) => f,
            None => continue,
        };
        if frag.token_count > remaining_budget {
            continue;
        }
        if selected_ids.overlaps(frag) {
            continue;
        }
        if entry.version < cv {
            let new_density = compute_density(
                frag,
                rel.get(&frag.id).copied().unwrap_or(0.0),
                needs,
                state,
            );
            heap.push(HeapEntry {
                neg_density: -new_density,
                frag_id: frag.id.clone(),
                version: cv,
            });
            continue;
        }
        let actual_density = -entry.neg_density;
        if actual_density <= 0.0 {
            return (None, 0.0, cv);
        }
        return (Some(frag.clone()), actual_density, cv + 1);
    }
    (None, 0.0, cv)
}

fn find_best_singleton(
    non_core: &[Fragment],
    base_selected_ids: &IntervalIndex,
    base_budget: u32,
    rel: &FxHashMap<FragmentId, f64>,
    needs: &[InformationNeed],
    base_state: &UtilityState,
) -> (Option<Fragment>, f64) {
    let mut best_singleton = None;
    let mut best_gain = 0.0;
    for f in non_core {
        if f.token_count > base_budget {
            continue;
        }
        if base_selected_ids.overlaps(f) {
            continue;
        }
        let gain = marginal_gain(f, rel.get(&f.id).copied().unwrap_or(0.0), needs, base_state);
        if gain > best_gain {
            best_gain = gain;
            best_singleton = Some(f.clone());
        }
    }
    (best_singleton, best_gain)
}

/// Paper-aligned strict Khuller H₁: `argmax_{f ∈ F, |f| ≤ B} U({f})`.
/// Iterates the FULL ground set (including core), gates on the FULL
/// budget B (not the residual after partial-core packing), and evaluates
/// each candidate as a lone selection against an empty utility state.
///
/// This is the comparator that, together with the greedy chain, gives
/// the `(1-1/e)/2` approximation guarantee from Khuller-Moss-Naor 1999
/// for monotone submodular maximization under a knapsack constraint.
/// Restricting H₁ to `non_core` with budget `B − cost(packed_core)`
/// (the prior `find_best_singleton`) is a strict relaxation that
/// excludes any single high-utility fragment with `|f| > B − β_core·B`.
fn find_best_singleton_full_set(
    fragments: &[Fragment],
    budget_tokens: u32,
    rel: &FxHashMap<FragmentId, f64>,
    needs: &[InformationNeed],
    empty_state: &UtilityState,
) -> (Option<Fragment>, f64) {
    let mut best = None;
    let mut best_gain = 0.0;
    for f in fragments {
        if f.token_count == 0 || f.token_count > budget_tokens {
            continue;
        }
        let gain = marginal_gain(
            f,
            rel.get(&f.id).copied().unwrap_or(0.0),
            needs,
            empty_state,
        );
        if gain > best_gain {
            best_gain = gain;
            best = Some(f.clone());
        }
    }
    (best, best_gain)
}

fn init_selection_state(
    core_ids: &FxHashSet<FragmentId>,
    rel: &FxHashMap<FragmentId, f64>,
    budget_tokens: u32,
    file_importance: Option<&FxHashMap<Arc<str>, f64>>,
) -> SelectionState {
    let mut utility_state = UtilityState::default();
    utility_state.r_cap = compute_r_cap(rel, Some(core_ids));
    utility_state.changed_dirs = core_ids
        .iter()
        .filter_map(|cid| {
            std::path::Path::new(cid.path.as_ref())
                .parent()
                .map(|p| p.to_path_buf())
        })
        .collect();
    if let Some(fi) = file_importance {
        utility_state.file_importance.clone_from(fi);
    }
    SelectionState {
        selected: Vec::new(),
        selected_ids: IntervalIndex::new(),
        remaining_budget: budget_tokens,
        utility_state,
    }
}

fn run_greedy_loop_heap(
    heap: &mut BinaryHeap<HeapEntry>,
    id_to_frag: &FxHashMap<FragmentId, Fragment>,
    state: &mut SelectionState,
    rel: &FxHashMap<FragmentId, f64>,
    needs: &[InformationNeed],
    tau: f64,
    _initial_budget: u32,
) -> (usize, f64, usize) {
    let mut current_version = 0u32;
    let mut peak_density: f64 = 0.0;
    let mut loop_iters: usize = 0;

    while !heap.is_empty() && state.remaining_budget > 0 {
        loop_iters += 1;
        let (best_frag, best_density, new_version) = find_best_candidate_heap(
            heap,
            current_version,
            id_to_frag,
            &state.selected_ids,
            state.remaining_budget,
            rel,
            needs,
            &state.utility_state,
        );
        current_version = new_version;

        let best_frag = match best_frag {
            Some(f) => f,
            None => break,
        };
        if best_density <= 0.0 {
            break;
        }

        if best_density > peak_density {
            peak_density = best_density;
        } else if peak_density > 0.0 && best_density < tau * peak_density {
            break;
        }

        state.selected.push(best_frag.clone());
        state.selected_ids.add_id(&best_frag.id);
        state.remaining_budget = state.remaining_budget.saturating_sub(best_frag.token_count);
        let rel_score = rel.get(&best_frag.id).copied().unwrap_or(0.0);
        apply_fragment(&best_frag, rel_score, needs, &mut state.utility_state);
    }

    let threshold = tau * peak_density;
    (state.selected.len(), threshold, loop_iters)
}

fn setup_and_select_core(
    fragments: &[Fragment],
    core_ids: &FxHashSet<FragmentId>,
    rel: &FxHashMap<FragmentId, f64>,
    needs: &[InformationNeed],
    budget_tokens: u32,
    file_importance: Option<&FxHashMap<Arc<str>, f64>>,
) -> (SelectionState, Vec<Fragment>, Vec<Fragment>, bool) {
    let mut core_fragments: Vec<Fragment> = fragments
        .iter()
        .filter(|f| core_ids.contains(&f.id))
        .cloned()
        .collect();
    core_fragments.sort_by(|a, b| {
        let ta = if a.token_count > 0 {
            a.token_count
        } else {
            SENTINEL_TOKEN_COUNT
        };
        let tb = if b.token_count > 0 {
            b.token_count
        } else {
            SENTINEL_TOKEN_COUNT
        };
        ta.cmp(&tb)
            .then(a.line_count().cmp(&b.line_count()))
            .then(a.start_line().cmp(&b.start_line()))
    });

    let non_core_fragments: Vec<Fragment> = fragments
        .iter()
        .filter(|f| !core_ids.contains(&f.id))
        .cloned()
        .collect();

    let sig_lookup = build_signature_lookup(fragments, &core_fragments);
    let mut state = init_selection_state(core_ids, rel, budget_tokens, file_importance);
    select_core_fragments(
        &core_fragments,
        rel,
        needs,
        &mut state,
        budget_tokens,
        &sig_lookup,
    );

    let selected_core_ids: FxHashSet<FragmentId> =
        state.selected.iter().map(|f| f.id.clone()).collect();
    let skipped_core: Vec<FragmentId> = core_ids
        .iter()
        .filter(|id| !selected_core_ids.contains(*id))
        .cloned()
        .collect();

    let mut non_core_with_skipped = non_core_fragments;
    if !skipped_core.is_empty() {
        let skipped_set: FxHashSet<FragmentId> = skipped_core.into_iter().collect();
        for cf in &core_fragments {
            if skipped_set.contains(&cf.id) {
                non_core_with_skipped.push(cf.clone());
            }
        }
    }

    let should_return_early = state.remaining_budget == 0;
    let selected_copy = state.selected.clone();
    (
        state,
        non_core_with_skipped,
        selected_copy,
        should_return_early,
    )
}

pub fn lazy_greedy_select(
    fragments: Vec<Fragment>,
    core_ids: &FxHashSet<FragmentId>,
    rel: &FxHashMap<FragmentId, f64>,
    needs: &[InformationNeed],
    budget_tokens: u32,
    tau: f64,
    file_importance: Option<&FxHashMap<Arc<str>, f64>>,
) -> SelectionResult {
    if fragments.is_empty() {
        return SelectionResult {
            selected: Vec::new(),
            reason: SelectionReason::NoCandidates,
            used_tokens: 0,
            utility: 0.0,
            greedy_iters: 0,
        };
    }

    let (mut state, non_core_fragments, _selected_core, should_return_early) =
        setup_and_select_core(
            &fragments,
            core_ids,
            rel,
            needs,
            budget_tokens,
            file_importance,
        );

    if should_return_early {
        let used = budget_tokens - state.remaining_budget;
        return SelectionResult {
            selected: state.selected,
            reason: SelectionReason::BudgetExhausted,
            used_tokens: used,
            utility: utility_value(&state.utility_state),
            greedy_iters: 0,
        };
    }

    let base_state = state.utility_state.copy();
    let base_selected = state.selected.clone();
    let base_budget = state.remaining_budget;

    let candidates: Vec<Fragment> = non_core_fragments
        .iter()
        .filter(|f| !state.selected_ids.overlaps(f))
        .cloned()
        .collect();
    let candidates = drop_redundant_signatures(&candidates, state.remaining_budget);

    let mut id_to_frag: FxHashMap<FragmentId, Fragment> = FxHashMap::default();
    let mut heap = build_initial_heap(
        &candidates,
        rel,
        needs,
        &state.utility_state,
        &mut id_to_frag,
    );

    let (_, threshold, greedy_iters) = run_greedy_loop_heap(
        &mut heap,
        &id_to_frag,
        &mut state,
        rel,
        needs,
        tau,
        budget_tokens,
    );

    let greedy_utility = utility_value(&state.utility_state);

    let mut base_selected_ids = IntervalIndex::new();
    for f in &base_selected {
        base_selected_ids.add_id(&f.id);
    }

    let (best_singleton, best_gain) = find_best_singleton(
        &non_core_fragments,
        &base_selected_ids,
        base_budget,
        rel,
        needs,
        &base_state,
    );

    let empty_state = init_selection_state(core_ids, rel, budget_tokens, file_importance);
    let (full_singleton, full_singleton_gain) = find_best_singleton_full_set(
        &fragments,
        budget_tokens,
        rel,
        needs,
        &empty_state.utility_state,
    );

    let mut best_alt_utility = greedy_utility;
    let mut best_alt: Option<(Vec<Fragment>, u32)> = None;

    if let Some(ref singleton) = best_singleton {
        let u = utility_value(&base_state) + best_gain;
        if u > best_alt_utility {
            best_alt_utility = u;
            let used = budget_tokens - (base_budget - singleton.token_count);
            let mut sel = base_selected.clone();
            sel.push(singleton.clone());
            best_alt = Some((sel, used));
        }
    }

    if let Some(ref full) = full_singleton {
        let u = utility_value(&empty_state.utility_state) + full_singleton_gain;
        if u > best_alt_utility {
            best_alt_utility = u;
            best_alt = Some((vec![full.clone()], full.token_count));
        }
    }

    if let Some((sel, used)) = best_alt {
        return SelectionResult {
            selected: sel,
            reason: SelectionReason::BestSingleton,
            used_tokens: used,
            utility: best_alt_utility,
            greedy_iters,
        };
    }

    let used = budget_tokens - state.remaining_budget;
    let reason = if state.remaining_budget == 0 {
        SelectionReason::BudgetExhausted
    } else if greedy_utility <= 0.0 {
        SelectionReason::NoUtility
    } else if state.selected.is_empty() || state.selected.len() == base_selected.len() {
        SelectionReason::NoCandidates
    } else if threshold > 0.0 && !heap.is_empty() {
        SelectionReason::StoppedByTau
    } else {
        SelectionReason::NoCandidates
    };

    SelectionResult {
        selected: state.selected,
        reason,
        used_tokens: used,
        utility: greedy_utility,
        greedy_iters,
    }
}
