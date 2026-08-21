use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::limits::UTILITY;
use crate::config::selection::selection;
use crate::interval::IntervalIndex;
use crate::types::{Fragment, FragmentId};
use crate::utility::needs::InformationNeed;
use crate::utility::objective::{
    UtilityState, apply_fragment, compute_density, marginal_gain, utility_value,
};

const SENTINEL_TOKEN_COUNT: u32 = 1_000_000_000;

/// `used_tokens` is a reported contract, so it is derived from the returned
/// selection rather than reconstructed from budget arithmetic that can drift
/// away from what was actually placed.
fn selection_cost(selected: &[Fragment]) -> u32 {
    selected.iter().map(|f| f.token_count).sum()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionReason {
    TopK,
    NoCandidates,
    BudgetExhausted,
    NoUtility,
    StoppedByTau,
    BestSingleton,
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
    /// Additive certificate for adaptive stopping: an upper bound
    /// (`tau * peak_density * remaining_budget`) on the utility that
    /// continuing the same greedy to the feasibility frontier could
    /// still have added. 0 when the loop ended for any other reason.
    pub stopping_certificate: f64,
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
        other
            .neg_density
            .total_cmp(&self.neg_density)
            .then_with(|| other.frag_id.cmp(&self.frag_id))
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
            // Keep the LARGEST co-located full fragment, not the last one seen.
            // Two non-signature fragments can share a start line (a class header
            // `Definition` at [10,12] and the full class at [10,300]), and with a
            // plain `insert` whichever came last in the candidate vec won the
            // slot. When the small header won, the class's stub was filtered out
            // as "redundant" precisely when the full class did not fit and the
            // stub was its only affordable representation — and the outcome
            // depended on vec order rather than on anything meaningful.
            full_token_by_loc
                .entry((f.id.path.clone(), f.start_line()))
                .and_modify(|t| *t = (*t).max(f.token_count))
                .or_insert(f.token_count);
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
    core_excerpts: Option<&FxHashMap<FragmentId, Fragment>>,
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
            continue;
        }
        // Kinds without a signature (chunk, section) fall back to the excerpt
        // around the hunk; without it an oversized core is skipped outright and
        // the change signal disappears from the output (#103).
        if let Some(excerpt) = core_excerpts.and_then(|e| e.get(&cf.id)) {
            sig_lookup.insert(cf.id.clone(), excerpt.clone());
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
    core_excerpts: Option<&FxHashMap<FragmentId, Fragment>>,
) -> FxHashSet<FragmentId> {
    // Which cores came out represented — by themselves, by a signature stub, or
    // by a downshifted excerpt. A substitute has its own id, so membership in
    // the selection cannot answer this, and treating a substituted core as
    // "skipped" hands the full fragment straight back to the greedy.
    let mut satisfied: FxHashSet<FragmentId> = FxHashSet::default();
    let sel_cfg = selection();
    let core_budget = (budget_tokens as f64 * sel_cfg.core_budget_fraction) as u32;
    // #194: while other cores still wait, one file may not eat more than its
    // share. The rescue sweep below runs ceiling-free — leftovers no other
    // file claimed flow back — so a single-file change never strands budget.
    let file_ceiling = (budget_tokens as f64 * sel_cfg.per_file_budget_fraction) as u32;
    let mut file_spent: FxHashMap<Arc<str>, u32> = FxHashMap::default();
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

    let place_fragment = |frag: &Fragment,
                          core_used: &mut u32,
                          state: &mut SelectionState,
                          rel_score: f64,
                          file_spent: &mut FxHashMap<Arc<str>, u32>| {
        state.selected.push(frag.clone());
        state.selected_ids.add_id(&frag.id);
        state.remaining_budget = state.remaining_budget.saturating_sub(frag.token_count);
        *core_used += frag.token_count;
        *file_spent.entry(frag.id.path.clone()).or_insert(0) += frag.token_count;
        apply_fragment(frag, rel_score, needs, &mut state.utility_state);
    };

    // (originating core id, the fragment actually offered for it — the core
    // itself or its downshifted excerpt).
    let mut skipped: Vec<(FragmentId, &Fragment)> = Vec::new();
    for frag in &sorted_core {
        // Downshift before the budget is consulted, not only when it forces the
        // issue. A core whose hunk window covers a small share of it is mostly
        // unchanged context, and emitting it whole is the over-dump behind
        // #105/#107/#149 — behaviour that otherwise flips purely on how much
        // budget happens to be left.
        let core_id = frag.id.clone();
        let frag: &Fragment = core_excerpts
            .and_then(|e| e.get(&frag.id))
            .filter(|excerpt| crate::excerpt::is_downshift_worthwhile(frag, excerpt))
            .unwrap_or(frag);
        if state.selected_ids.is_superset_of(frag) {
            satisfied.insert(core_id);
            continue;
        }
        let spent = file_spent.get(&frag.id.path).copied().unwrap_or(0);
        let over_file_ceiling = spent > 0 && spent + frag.token_count > file_ceiling;
        if core_used + frag.token_count > core_budget || over_file_ceiling {
            if let Some(sig) = sig_lookup.get(&core_id) {
                // The substitute honors the same per-file ceiling (#212): a
                // file at its share must not keep placing signature stubs —
                // its remaining cores defer to the ceiling-free rescue sweep
                // below, after other files had their turn.
                let sig_over_ceiling = spent > 0 && spent + sig.token_count > file_ceiling;
                if !state.selected_ids.contains(&sig.id)
                    && core_used + sig.token_count <= core_budget
                    && !sig_over_ceiling
                {
                    let rel_score = rel.get(&core_id).copied().unwrap_or(0.0);
                    place_fragment(sig, &mut core_used, state, rel_score, &mut file_spent);
                    satisfied.insert(core_id);
                    continue;
                }
            }
            skipped.push((core_id, frag));
            continue;
        }

        let rel_score = rel.get(&core_id).copied().unwrap_or(0.0);
        place_fragment(frag, &mut core_used, state, rel_score, &mut file_spent);
        satisfied.insert(core_id);
    }

    // Bug #2 fix: cores that didn't fit the core_budget reservation must not be
    // demoted to ordinary greedy candidates without a chance to be placed first.
    // Sweep skipped cores cheapest-first against the *full* remaining budget
    // (not just the core slice) so seeds aren't dropped purely because the
    // highest-relevance core happened to be heavy.
    if !skipped.is_empty() {
        skipped.sort_by(|(_, a), (_, b)| a.token_count.cmp(&b.token_count));
        for (core_id, frag) in skipped {
            if state.remaining_budget == 0 {
                break;
            }
            if state.selected_ids.is_superset_of(frag) {
                satisfied.insert(core_id);
                continue;
            }
            let rel_score = rel.get(&core_id).copied().unwrap_or(0.0);
            if frag.token_count <= state.remaining_budget {
                place_fragment(frag, &mut core_used, state, rel_score, &mut file_spent);
                satisfied.insert(core_id);
            } else if let Some(sig) = sig_lookup.get(&core_id) {
                if !state.selected_ids.contains(&sig.id)
                    && sig.token_count <= state.remaining_budget
                {
                    place_fragment(sig, &mut core_used, state, rel_score, &mut file_spent);
                    satisfied.insert(core_id);
                }
            }
        }
    }

    satisfied
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
    open_paths: &FxHashSet<Arc<str>>,
    admissible_files: Option<&FxHashSet<Arc<str>>>,
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
        if !file_admitted(&f.id.path, open_paths, admissible_files) {
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
    open_paths: &FxHashSet<Arc<str>>,
    admissible_files: Option<&FxHashSet<Arc<str>>>,
) -> (Option<Fragment>, f64) {
    let mut best = None;
    let mut best_gain = 0.0;
    for f in fragments {
        if f.token_count == 0 || f.token_count > budget_tokens {
            continue;
        }
        if !file_admitted(&f.id.path, open_paths, admissible_files) {
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

#[allow(clippy::too_many_arguments)]
fn run_greedy_loop_heap(
    heap: &mut BinaryHeap<HeapEntry>,
    id_to_frag: &FxHashMap<FragmentId, Fragment>,
    state: &mut SelectionState,
    rel: &FxHashMap<FragmentId, f64>,
    needs: &[InformationNeed],
    tau: f64,
    _initial_budget: u32,
    admissible_files: Option<&FxHashSet<Arc<str>>>,
) -> (usize, f64, usize) {
    let mut current_version = 0u32;
    let mut peak_density: f64 = 0.0;
    let mut loop_iters: usize = 0;
    // Per-file admission (#65): opening a NEW file requires it to be
    // naming-reachable from the changed set; fragments of files already
    // opened (including the cores selected before this loop) compete on
    // density as always. Inadmissible candidates are discarded, not
    // deferred — admissibility is static within a run.
    let mut open_files: FxHashSet<Arc<str>> =
        state.selected.iter().map(|f| f.id.path.clone()).collect();
    // #194 per-file ceiling: while the heap still holds other candidates, one
    // file may not exceed its budget share. Blocked candidates are deferred,
    // not dropped — once everyone else has had their chance the ceiling lifts
    // (phase 2), so leftovers still flow to the highest density and a
    // single-file run is unaffected.
    let file_ceiling = (_initial_budget as f64 * selection().per_file_budget_fraction) as u32;
    let mut file_spent: FxHashMap<Arc<str>, u32> = FxHashMap::default();
    for f in &state.selected {
        *file_spent.entry(f.id.path.clone()).or_insert(0) += f.token_count;
    }
    let mut deferred: Vec<HeapEntry> = Vec::new();
    let mut ceiling_active = true;

    loop {
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

            if !file_admitted(&best_frag.id.path, &open_files, admissible_files) {
                continue;
            }

            // A deferred candidate still sets the peak (#197): it was the
            // legitimate argmax, and excluding it calibrated `tau * peak` on
            // whatever foreign-file fragment came next — the ceiling is a
            // budget constraint and must not silently act as a scoring one.
            if best_density > peak_density {
                peak_density = best_density;
            }

            if ceiling_active {
                let spent = file_spent.get(&best_frag.id.path).copied().unwrap_or(0);
                if spent > 0 && spent + best_frag.token_count > file_ceiling {
                    deferred.push(HeapEntry {
                        neg_density: -best_density,
                        frag_id: best_frag.id.clone(),
                        version: current_version,
                    });
                    continue;
                }
            }

            if peak_density > 0.0 && best_density < tau * peak_density {
                break;
            }

            open_files.insert(best_frag.id.path.clone());
            *file_spent.entry(best_frag.id.path.clone()).or_insert(0) += best_frag.token_count;
            state.selected.push(best_frag.clone());
            state.selected_ids.add_id(&best_frag.id);
            state.remaining_budget = state.remaining_budget.saturating_sub(best_frag.token_count);
            let rel_score = rel.get(&best_frag.id).copied().unwrap_or(0.0);
            apply_fragment(&best_frag, rel_score, needs, &mut state.utility_state);
        }

        if ceiling_active && !deferred.is_empty() && state.remaining_budget > 0 {
            ceiling_active = false;
            for e in deferred.drain(..) {
                heap.push(e);
            }
            continue;
        }
        break;
    }

    let threshold = tau * peak_density;
    (state.selected.len(), threshold, loop_iters)
}

/// #65 admission for every selection path, not only the greedy loop (#211):
/// a fragment may open a NEW file only if that file is naming-reachable from
/// the changed set. Files already open keep competing, and `None` means the
/// gate is disabled (BM25 mode, or `DIFFCTX_FILE_ADMISSION=0`).
fn file_admitted(
    path: &Arc<str>,
    open_paths: &FxHashSet<Arc<str>>,
    admissible: Option<&FxHashSet<Arc<str>>>,
) -> bool {
    admissible.is_none_or(|a| open_paths.contains(path) || a.contains(path))
}

fn setup_and_select_core(
    fragments: &[Fragment],
    core_ids: &FxHashSet<FragmentId>,
    rel: &FxHashMap<FragmentId, f64>,
    needs: &[InformationNeed],
    budget_tokens: u32,
    file_importance: Option<&FxHashMap<Arc<str>, f64>>,
    core_excerpts: Option<&FxHashMap<FragmentId, Fragment>>,
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

    let sig_lookup = build_signature_lookup(fragments, &core_fragments, core_excerpts);
    let mut state = init_selection_state(core_ids, rel, budget_tokens, file_importance);
    let satisfied_core_ids = select_core_fragments(
        &core_fragments,
        rel,
        needs,
        &mut state,
        budget_tokens,
        &sig_lookup,
        core_excerpts,
    );

    // A core represented by a substitute (signature stub or downshifted
    // excerpt) is satisfied even though its own id is absent from the
    // selection — offering the full fragment back to the greedy would undo the
    // substitution.
    let skipped_core: Vec<FragmentId> = core_ids
        .iter()
        .filter(|id| !satisfied_core_ids.contains(*id))
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
    core_excerpts: Option<&FxHashMap<FragmentId, Fragment>>,
    admissible_files: Option<&FxHashSet<Arc<str>>>,
    declared_admissible_files: Option<&FxHashSet<Arc<str>>>,
) -> SelectionResult {
    if fragments.is_empty() {
        return SelectionResult {
            selected: Vec::new(),
            reason: SelectionReason::NoCandidates,
            used_tokens: 0,
            utility: 0.0,
            greedy_iters: 0,
            stopping_certificate: 0.0,
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
            core_excerpts,
        );

    if should_return_early {
        let used = budget_tokens - state.remaining_budget;
        return SelectionResult {
            selected: state.selected,
            reason: SelectionReason::BudgetExhausted,
            used_tokens: used,
            utility: utility_value(&state.utility_state),
            greedy_iters: 0,
            stopping_certificate: 0.0,
        };
    }

    let base_state = state.utility_state.clone();
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
        admissible_files,
    );

    let greedy_utility = utility_value(&state.utility_state);

    let mut base_selected_ids = IntervalIndex::new();
    for f in &base_selected {
        base_selected_ids.add_id(&f.id);
    }

    let base_open_paths: FxHashSet<Arc<str>> =
        base_selected.iter().map(|f| f.id.path.clone()).collect();
    let (best_singleton, best_gain) = find_best_singleton(
        &non_core_fragments,
        &base_selected_ids,
        base_budget,
        rel,
        needs,
        &base_state,
        &base_open_paths,
        declared_admissible_files,
    );

    let empty_state = init_selection_state(core_ids, rel, budget_tokens, file_importance);
    let (full_singleton, full_singleton_gain) = find_best_singleton_full_set(
        &fragments,
        budget_tokens,
        rel,
        needs,
        &empty_state.utility_state,
        &base_open_paths,
        declared_admissible_files,
    );

    let mut best_alt_utility = greedy_utility;
    let mut best_alt: Option<(u32, Vec<Fragment>)> = None;

    if let Some(ref singleton) = best_singleton {
        let u = utility_value(&base_state) + best_gain;
        if u > best_alt_utility {
            best_alt_utility = u;
            let mut sel = base_selected.clone();
            sel.push(singleton.clone());
            best_alt = Some((selection_cost(&sel), sel));
        }
    }

    if let Some(ref full) = full_singleton {
        let u = utility_value(&empty_state.utility_state) + full_singleton_gain;
        // Additive on top of the core selection, never a replacement for it.
        // This branch used to return `vec![full]` outright, so a single heavy
        // fragment whose standalone utility beat the greedy chain's discarded
        // every changed-code fragment — the one thing the output exists to
        // carry. `ensure_changed_files_represented` could not reliably undo it
        // either: it only had `budget - full.token_count` left and only picks a
        // fragment that fits. The two utilities are also measured from
        // different baselines (this one from an empty state, `greedy_utility`
        // from the core base), so the comparison can only ever be a heuristic
        // nudge — not grounds for dropping the core.
        //
        // H₁ iterates the FULL ground set, so its winner can be a core the
        // core pass already packed. Then this arm has nothing to add: its
        // "alternative" is `base_selected` verbatim, a strict subset of the
        // greedy result. Utility is monotone and `greedy_utility` already
        // contains that core's contribution, so `u > best_alt_utility` should
        // be unreachable in that case — this makes the reasoning a condition
        // rather than an assumption, because the arm's cost accounting has no
        // meaning when nothing is appended.
        let already_selected = base_selected.iter().any(|f| f.id == full.id);
        if u > best_alt_utility && full.token_count <= base_budget && !already_selected {
            best_alt_utility = u;
            let mut sel = base_selected.clone();
            sel.push(full.clone());
            best_alt = Some((selection_cost(&sel), sel));
        }
    }

    if let Some((used, sel)) = best_alt {
        return SelectionResult {
            selected: sel,
            reason: SelectionReason::BestSingleton,
            used_tokens: used,
            utility: best_alt_utility,
            greedy_iters,
            stopping_certificate: 0.0,
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

    let stopping_certificate = if matches!(reason, SelectionReason::StoppedByTau) {
        threshold * f64::from(state.remaining_budget)
    } else {
        0.0
    };

    SelectionResult {
        selected: state.selected,
        reason,
        used_tokens: used,
        utility: greedy_utility,
        greedy_iters,
        stopping_certificate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FragmentKind;

    fn frag(path: &str, start: u32, end: u32, kind: FragmentKind, tokens: u32) -> Fragment {
        let mut identifiers = FxHashSet::default();
        identifiers.insert(format!("sym_{path}_{start}"));
        Fragment {
            id: FragmentId::new(Arc::from(path), start, end),
            kind,
            content: Arc::from(format!("// {path}:{start}-{end}\n")),
            identifiers,
            token_count: tokens,
            symbol_name: Some(format!("sym_{start}")),
        }
    }

    fn rel_map(frags: &[Fragment], score: f64) -> FxHashMap<FragmentId, f64> {
        frags.iter().map(|f| (f.id.clone(), score)).collect()
    }

    fn cost_of(selected: &[Fragment]) -> u32 {
        selected.iter().map(|f| f.token_count).sum()
    }

    /// The budget is a hard contract (`cost(C) <= B`); four separate call sites
    /// gate on it and none of them was asserted. A `pick_smallest_fitting` that
    /// returns a non-fitting candidate is one `if` away, and the oracle corpus
    /// can never catch it because its budget is always >=2.5x the whole repo.
    #[test]
    fn selection_never_exceeds_the_budget() {
        let frags: Vec<Fragment> = (0..12)
            .map(|i| {
                frag(
                    "a.rs",
                    1 + i * 50,
                    40 + i * 50,
                    FragmentKind::Function,
                    30 + i * 17,
                )
            })
            .collect();
        let core: FxHashSet<FragmentId> = std::iter::once(frags[0].id.clone()).collect();
        let rel = rel_map(&frags, 0.7);

        for budget in [1u32, 7, 30, 31, 60, 200, 1_000] {
            let result = lazy_greedy_select(
                frags.clone(),
                &core,
                &rel,
                &[],
                budget,
                0.12,
                None,
                None,
                None,
                None,
            );
            assert!(
                cost_of(&result.selected) <= budget,
                "budget {budget} overrun: cost {} via {:?}",
                cost_of(&result.selected),
                result.reason
            );
            assert!(
                result.used_tokens <= budget,
                "reported used_tokens {} exceeds budget {budget}",
                result.used_tokens
            );
        }
    }

    /// `used_tokens` is what every downstream budget report reads, and it was
    /// only ever asserted as `<= budget`. Three code paths compute it by
    /// different budget arithmetic; this pins the equality they all have to
    /// satisfy, so a future path that reconstructs the figure instead of
    /// measuring the selection fails here rather than in a results table.
    #[test]
    fn reported_used_tokens_always_equals_the_cost_of_the_returned_selection() {
        let shapes: Vec<Vec<Fragment>> = vec![
            vec![
                frag("changed.rs", 1, 8, FragmentKind::Function, 20),
                frag("other.rs", 1, 400, FragmentKind::Class, 900),
                frag("other.rs", 500, 520, FragmentKind::Function, 40),
            ],
            // A lone, heavy core: H₁ over the full ground set can only win with
            // a fragment the core pass already placed.
            vec![frag("changed.rs", 1, 200, FragmentKind::Class, 400)],
            (0..6)
                .map(|i| {
                    frag(
                        "a.rs",
                        1 + i * 20,
                        10 + i * 20,
                        FragmentKind::Function,
                        20 + i * 60,
                    )
                })
                .collect(),
        ];

        for frags in shapes {
            let core: FxHashSet<FragmentId> = std::iter::once(frags[0].id.clone()).collect();
            let rel = rel_map(&frags, 0.8);
            for budget in [50u32, 120, 460, 1_000, 5_000] {
                let result = lazy_greedy_select(
                    frags.clone(),
                    &core,
                    &rel,
                    &[],
                    budget,
                    0.12,
                    None,
                    None,
                    None,
                    None,
                );
                assert_eq!(
                    result.used_tokens,
                    cost_of(&result.selected),
                    "reason {:?} at budget {budget}: reported {} but selection costs {}",
                    result.reason,
                    result.used_tokens,
                    cost_of(&result.selected)
                );
            }
        }
    }

    /// A core that is mostly unchanged must be placed as its hunk-window
    /// excerpt, not in full — the over-dump behind #105/#107/#149. The excerpt
    /// arrives through `core_excerpts`, keyed by the core it replaces.
    #[test]
    fn a_mostly_unchanged_core_is_placed_as_its_excerpt() {
        let core = frag("script.sh", 1, 122, FragmentKind::Chunk, 600);
        let excerpt = frag("script.sh", 58, 64, FragmentKind::Excerpt, 40);
        let core_ids: FxHashSet<FragmentId> = std::iter::once(core.id.clone()).collect();
        let rel = rel_map(&[core.clone()], 1.0);
        let mut excerpts: FxHashMap<FragmentId, Fragment> = FxHashMap::default();
        excerpts.insert(core.id.clone(), excerpt.clone());

        let result = lazy_greedy_select(
            vec![core.clone()],
            &core_ids,
            &rel,
            &[],
            8_000,
            0.12,
            None,
            Some(&excerpts),
            None,
            None,
        );

        let ids: Vec<String> = result
            .selected
            .iter()
            .map(|f| format!("{}:{}-{}", f.id.path, f.id.start_line, f.id.end_line))
            .collect();
        assert!(
            result.selected.iter().any(|f| f.id == excerpt.id),
            "core was not downshifted to its excerpt: {ids:?}"
        );
        assert!(
            !result.selected.iter().any(|f| f.id == core.id),
            "the full core was emitted alongside the excerpt: {ids:?}"
        );
    }

    #[test]
    fn selected_fragments_never_overlap_and_are_never_duplicated() {
        let frags: Vec<Fragment> = (0..8)
            .map(|i| frag("a.rs", 1 + i * 10, 12 + i * 10, FragmentKind::Function, 25))
            .collect();
        let core: FxHashSet<FragmentId> = std::iter::once(frags[0].id.clone()).collect();
        let rel = rel_map(&frags, 0.9);
        let result = lazy_greedy_select(frags, &core, &rel, &[], 400, 0.12, None, None, None, None);

        let ids: FxHashSet<FragmentId> = result.selected.iter().map(|f| f.id.clone()).collect();
        assert_eq!(
            ids.len(),
            result.selected.len(),
            "duplicate fragment selected"
        );
    }

    /// `find_best_singleton_full_set` used to return `vec![full]`, discarding
    /// every core fragment. The core IS the changed code, so a selection that
    /// drops all of it answers a different question than the one asked.
    #[test]
    fn a_winning_singleton_never_evicts_the_core_selection() {
        // A heavy, highly relevant non-core fragment is the shape that makes the
        // full-set singleton win.
        let core_frag = frag("changed.rs", 1, 8, FragmentKind::Function, 20);
        let heavy = frag("other.rs", 1, 400, FragmentKind::Class, 900);
        let filler = frag("other.rs", 500, 520, FragmentKind::Function, 40);
        let frags = vec![core_frag.clone(), heavy.clone(), filler];

        let core: FxHashSet<FragmentId> = std::iter::once(core_frag.id.clone()).collect();
        let mut rel = FxHashMap::default();
        rel.insert(core_frag.id.clone(), 0.05);
        rel.insert(heavy.id.clone(), 1.0);
        rel.insert(frags[2].id.clone(), 0.1);

        let result =
            lazy_greedy_select(frags, &core, &rel, &[], 2_000, 0.12, None, None, None, None);
        assert!(
            result.selected.iter().any(|f| core.contains(&f.id)),
            "no core fragment survived; reason was {:?}",
            result.reason
        );
        assert!(cost_of(&result.selected) <= 2_000);
    }

    #[test]
    fn empty_ground_set_reports_no_candidates() {
        let result = lazy_greedy_select(
            Vec::new(),
            &FxHashSet::default(),
            &FxHashMap::default(),
            &[],
            1_000,
            0.12,
            None,
            None,
            None,
            None,
        );
        assert!(result.selected.is_empty());
        assert_eq!(result.reason, SelectionReason::NoCandidates);
        assert_eq!(result.used_tokens, 0);
    }

    /// Keyed on `(path, start_line)`, this used to be last-write-wins, so a
    /// small co-located sibling could delete the stub that was the only
    /// affordable representation of an oversized fragment.
    #[test]
    fn drop_redundant_signatures_is_independent_of_candidate_order() {
        let header = frag("a.rs", 10, 12, FragmentKind::Definition, 40);
        let whole = frag("a.rs", 10, 300, FragmentKind::Class, 4_000);
        let stub = frag("a.rs", 10, 11, FragmentKind::ClassSignature, 15);

        let forward =
            drop_redundant_signatures(&[header.clone(), whole.clone(), stub.clone()], 500);
        let backward = drop_redundant_signatures(&[whole, header, stub], 500);

        let kinds = |v: &[Fragment]| -> Vec<FragmentKind> { v.iter().map(|f| f.kind).collect() };
        assert!(
            kinds(&forward).contains(&FragmentKind::ClassSignature),
            "the stub for an unaffordable class was dropped: {:?}",
            kinds(&forward)
        );
        let mut a: Vec<FragmentKind> = kinds(&forward);
        let mut b: Vec<FragmentKind> = kinds(&backward);
        a.sort_by_key(|k| format!("{k:?}"));
        b.sort_by_key(|k| format!("{k:?}"));
        assert_eq!(a, b, "verdict depended on candidate order");
    }

    #[test]
    fn drop_redundant_signatures_removes_a_stub_whose_full_fragment_fits() {
        let whole = frag("a.rs", 10, 40, FragmentKind::Class, 100);
        let stub = frag("a.rs", 10, 11, FragmentKind::ClassSignature, 15);
        let kept = drop_redundant_signatures(&[whole, stub], 500);
        assert!(
            !kept.iter().any(|f| f.kind.is_signature()),
            "stub survived even though the full fragment fits the budget"
        );
    }
    /// tau is the adaptive stop: once a candidate's density falls below
    /// `tau * peak_density` the loop stops instead of spending the rest of the
    /// budget. The oracle corpus used to run at tau=0.0, which made the
    /// predicate unreachable, so deleting the rule failed no test; the corpus
    /// now runs at the shipped default too (#175). This keeps a direct
    /// assertion on the rule that does not depend on corpus wiring.
    #[test]
    fn tau_stops_the_greedy_loop_before_the_budget_is_spent() {
        // Descending relevance against escalating cost gives sharply
        // descending density, which is what the stop rule reacts to.
        let frags: Vec<Fragment> = (0..6)
            .map(|i| {
                frag(
                    "a.rs",
                    1 + i * 20,
                    10 + i * 20,
                    FragmentKind::Function,
                    20 + i * i * 120,
                )
            })
            .collect();
        let core: FxHashSet<FragmentId> = std::iter::once(frags[0].id.clone()).collect();
        let mut rel = FxHashMap::default();
        // Geometric decay: with escalating cost the density ratio between
        // consecutive candidates falls below 5% by the tail, so the rule
        // fires at the shipped tau (0.05) and not only at looser settings.
        for (i, f) in frags.iter().enumerate() {
            rel.insert(f.id.clone(), 0.25f64.powi(i as i32).max(1e-6));
        }

        let budget = 10_000;
        let default_tau = lazy_greedy_select(
            frags.clone(),
            &core,
            &rel,
            &[],
            budget,
            crate::config::limits::DEFAULT_STOPPING_THRESHOLD,
            None,
            None,
            None,
            None,
        );
        let no_tau =
            lazy_greedy_select(frags, &core, &rel, &[], budget, 0.0, None, None, None, None);

        assert_eq!(
            default_tau.reason,
            SelectionReason::StoppedByTau,
            "the adaptive stop did not fire at the shipped default"
        );
        assert!(
            default_tau.selected.len() < no_tau.selected.len(),
            "tau={} selected {} fragments, same as tau=0.0 — the rule is inert",
            crate::config::limits::DEFAULT_STOPPING_THRESHOLD,
            default_tau.selected.len()
        );
        assert!(
            default_tau.used_tokens < no_tau.used_tokens,
            "the stop saved no budget: {} vs {}",
            default_tau.used_tokens,
            no_tau.used_tokens
        );
        assert!(
            default_tau.stopping_certificate > 0.0,
            "StoppedByTau must carry a positive certificate"
        );
        assert_eq!(
            no_tau.stopping_certificate, 0.0,
            "tau=0.0 cannot produce a stopping certificate"
        );
    }

    /// #194: with competitors waiting, one file may not monopolize the budget
    /// — but once every other file has had its chance, the ceiling lifts and
    /// leftovers flow back, so a lone file still fills the budget.
    #[test]
    fn per_file_ceiling_blocks_monopoly_but_releases_leftovers() {
        // One "blob" file with many equal fragments vs two small files.
        let mut frags: Vec<Fragment> = (0..20)
            .map(|i| {
                frag(
                    "blob.json",
                    i * 10 + 1,
                    i * 10 + 9,
                    FragmentKind::Chunk,
                    100,
                )
            })
            .collect();
        frags.push(frag("a.rs", 1, 9, FragmentKind::Function, 100));
        frags.push(frag("b.rs", 1, 9, FragmentKind::Function, 100));
        let core: FxHashSet<FragmentId> = FxHashSet::default();
        let mut rel: FxHashMap<FragmentId, f64> = FxHashMap::default();
        // Blob fragments outrank the small files.
        for f in &frags {
            let w = if f.id.path.as_ref() == "blob.json" {
                1.0
            } else {
                0.5
            };
            rel.insert(f.id.clone(), w);
        }
        let budget = 1_000u32; // ceiling = 250 -> 2 blob frags in phase 1
        let result = lazy_greedy_select(
            frags.clone(),
            &core,
            &rel,
            &[],
            budget,
            0.0,
            None,
            None,
            None,
            None,
        );
        let by_file =
            |sel: &Vec<Fragment>, p: &str| sel.iter().filter(|f| f.id.path.as_ref() == p).count();
        assert!(
            by_file(&result.selected, "a.rs") == 1 && by_file(&result.selected, "b.rs") == 1,
            "small files must not be crowded out: {:?}",
            result
                .selected
                .iter()
                .map(|f| f.id.path.to_string())
                .collect::<Vec<_>>()
        );
        assert!(
            by_file(&result.selected, "blob.json") >= 5,
            "leftover budget must flow back to the blob once competitors are served"
        );

        run_lone_file_check(budget, &rel);
    }

    /// #212: once a file is at its ceiling, its remaining cores may not keep
    /// placing signature stubs either — competitor cores get their phase-1
    /// seat first, and the file's tail waits for the ceiling-free sweep.
    #[test]
    fn signature_stubs_honor_the_per_file_ceiling() {
        let budget = 1_000u32;
        let mut cores: Vec<Fragment> = vec![frag("blob.rs", 1, 90, FragmentKind::Function, 240)];
        for i in 1..8 {
            cores.push(frag(
                "blob.rs",
                1 + i * 100,
                90 + i * 100,
                FragmentKind::Function,
                400,
            ));
        }
        cores.push(frag("b.rs", 1, 50, FragmentKind::Function, 200));
        let sig_lookup: FxHashMap<FragmentId, Fragment> = cores
            .iter()
            .filter(|c| c.id.path.as_ref() == "blob.rs")
            .map(|c| {
                let mut sig = frag(
                    "blob.rs",
                    c.start_line(),
                    c.start_line(),
                    FragmentKind::FunctionSignature,
                    30,
                );
                sig.id = FragmentId::new(c.id.path.clone(), c.start_line(), c.start_line());
                (c.id.clone(), sig)
            })
            .collect();
        let mut rel: FxHashMap<FragmentId, f64> = FxHashMap::default();
        for c in &cores {
            let w = if c.id.path.as_ref() == "blob.rs" {
                1.0
            } else {
                0.5
            };
            rel.insert(c.id.clone(), w);
        }
        let core_ids: FxHashSet<FragmentId> = cores.iter().map(|c| c.id.clone()).collect();
        let mut state = init_selection_state(&core_ids, &rel, budget, None);
        select_core_fragments(&cores, &rel, &[], &mut state, budget, &sig_lookup, None);

        let first_b = state
            .selected
            .iter()
            .position(|f| f.id.path.as_ref() == "b.rs")
            .expect("the competitor core must be selected");
        let stubs_before_b = state.selected[..first_b]
            .iter()
            .filter(|f| f.kind.is_signature())
            .count();
        assert_eq!(
            stubs_before_b, 0,
            "a file at its ceiling placed {stubs_before_b} signature stubs before the competitor got its seat"
        );
    }

    fn run_lone_file_check(budget: u32, rel: &FxHashMap<FragmentId, f64>) {
        // Lone-file run: ceiling must not strand budget.
        let lone: Vec<Fragment> = (0..20)
            .map(|i| {
                frag(
                    "blob.json",
                    i * 10 + 1,
                    i * 10 + 9,
                    FragmentKind::Chunk,
                    100,
                )
            })
            .collect();
        let core: FxHashSet<FragmentId> = FxHashSet::default();
        let result = lazy_greedy_select(lone, &core, rel, &[], budget, 0.0, None, None, None, None);
        assert!(
            result.selected.len() >= 9,
            "single-file selection stranded budget: {} fragments",
            result.selected.len()
        );
    }
}
