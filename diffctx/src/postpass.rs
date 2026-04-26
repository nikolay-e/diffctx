use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::fragmentation::create_whole_file_fragment;
use crate::git::CatFileBatch;
use crate::graph::Graph;
use crate::types::{Fragment, FragmentId};

const RESCUE_BUDGET_FRACTION: f64 = 0.05;
const RESCUE_MIN_SCORE_PERCENTILE: f64 = 0.80;

struct IntervalIndex {
    by_path: HashMap<Arc<str>, Vec<(u32, u32)>>,
    ids: FxHashSet<FragmentId>,
}

impl IntervalIndex {
    fn new() -> Self {
        Self {
            by_path: HashMap::new(),
            ids: FxHashSet::default(),
        }
    }

    fn add(&mut self, frag: &Fragment) {
        self.ids.insert(frag.id.clone());
        let intervals = self.by_path.entry(frag.id.path.clone()).or_default();
        let pair = (frag.start_line(), frag.end_line());
        let pos = intervals
            .binary_search_by(|p| p.0.cmp(&pair.0).then(p.1.cmp(&pair.1)))
            .unwrap_or_else(|i| i);
        intervals.insert(pos, pair);
    }

    fn overlaps(&self, frag: &Fragment) -> bool {
        let intervals = match self.by_path.get(&frag.id.path) {
            Some(i) => i,
            None => return false,
        };
        for &(start, end) in intervals {
            if start == frag.start_line() && end == frag.end_line() {
                continue;
            }
            if end >= frag.start_line() && start <= frag.end_line() {
                return true;
            }
        }
        false
    }
}

fn find_dangling_semantic_names(
    selected: &[Fragment],
    graph: &Graph,
    frag_by_id: &FxHashMap<FragmentId, &Fragment>,
    selected_ids: &FxHashSet<FragmentId>,
) -> FxHashSet<String> {
    let mut dangling = FxHashSet::default();
    for frag in selected {
        graph.for_each_forward_neighbor(&frag.id, |nbr_id, _w| {
            if selected_ids.contains(nbr_id) {
                return;
            }
            let cat = graph
                .edge_categories
                .get(&(frag.id.clone(), nbr_id.clone()));
            if cat
                .map(|c| *c != crate::graph::EdgeCategory::Semantic)
                .unwrap_or(true)
            {
                return;
            }
            if let Some(nbr_frag) = frag_by_id.get(nbr_id) {
                if let Some(ref name) = nbr_frag.symbol_name {
                    dangling.insert(name.to_lowercase());
                }
            }
        });
    }
    dangling
}

fn pick_best_fragment<'a>(
    candidates: &[&'a Fragment],
    selected_ids: &FxHashSet<FragmentId>,
) -> Option<&'a Fragment> {
    if candidates.iter().any(|c| selected_ids.contains(&c.id)) {
        return None;
    }
    let full: Vec<&&Fragment> = candidates
        .iter()
        .filter(|f| !f.kind.is_signature())
        .collect();
    let sig: Vec<&&Fragment> = candidates
        .iter()
        .filter(|f| f.kind.is_signature())
        .collect();
    full.first().or(sig.first()).map(|f| **f)
}

fn pick_smallest_fitting(
    candidates: &[Fragment],
    selected_ids: &FxHashSet<FragmentId>,
    budget_left: u32,
) -> Option<Fragment> {
    let mut sorted: Vec<&Fragment> = candidates.iter().collect();
    sorted.sort_by_key(|f| f.token_count);
    for cand in sorted {
        if cand.token_count == 0 || selected_ids.contains(&cand.id) {
            continue;
        }
        if cand.token_count <= budget_left {
            return Some(cand.clone());
        }
    }
    None
}

pub fn coherence_post_pass(
    selected: &mut Vec<Fragment>,
    all_fragments: &[Fragment],
    graph: &Graph,
    budget: u32,
) {
    let selected_ids: FxHashSet<FragmentId> = selected.iter().map(|f| f.id.clone()).collect();
    let mut interval_idx = IntervalIndex::new();
    for f in selected.iter() {
        interval_idx.add(f);
    }
    let used: u32 = selected.iter().map(|f| f.token_count).sum();
    let mut remaining = budget.saturating_sub(used);

    let mut name_to_frags: HashMap<String, Vec<&Fragment>> = HashMap::new();
    for f in all_fragments {
        if let Some(ref name) = f.symbol_name {
            name_to_frags
                .entry(name.to_lowercase())
                .or_default()
                .push(f);
        }
    }

    let frag_by_id: FxHashMap<FragmentId, &Fragment> =
        all_fragments.iter().map(|f| (f.id.clone(), f)).collect();
    let dangling_names = find_dangling_semantic_names(selected, graph, &frag_by_id, &selected_ids);

    let mut added_ids = selected_ids;
    for name in &dangling_names {
        let candidates = match name_to_frags.get(name) {
            Some(c) => c,
            None => continue,
        };
        let pick = match pick_best_fragment(candidates, &added_ids) {
            Some(p) => p,
            None => continue,
        };
        if pick.token_count <= remaining
            && !added_ids.contains(&pick.id)
            && !interval_idx.overlaps(pick)
        {
            selected.push(pick.clone());
            added_ids.insert(pick.id.clone());
            interval_idx.add(pick);
            remaining = remaining.saturating_sub(pick.token_count);
        }
    }
}

fn compute_rescue_threshold(
    all_fragments: &[Fragment],
    rel_scores: &FxHashMap<FragmentId, f64>,
    core_ids: &FxHashSet<FragmentId>,
) -> f64 {
    let mut context_scores: Vec<f64> = all_fragments
        .iter()
        .filter(|f| !core_ids.contains(&f.id))
        .map(|f| rel_scores.get(&f.id).copied().unwrap_or(0.0))
        .filter(|&s| s > 0.0)
        .collect();
    if context_scores.is_empty() {
        return f64::INFINITY;
    }
    context_scores.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let idx = (context_scores.len() as f64 * (1.0 - RESCUE_MIN_SCORE_PERCENTILE)) as usize;
    context_scores[idx.min(context_scores.len() - 1)]
}

pub fn rescue_nontrivial_context(
    selected: &mut Vec<Fragment>,
    all_fragments: &[Fragment],
    rel_scores: &FxHashMap<FragmentId, f64>,
    core_ids: &FxHashSet<FragmentId>,
    budget: u32,
) {
    let used: u32 = selected.iter().map(|f| f.token_count).sum();
    let remaining = budget.saturating_sub(used);
    let rescue_budget = remaining.min((budget as f64 * RESCUE_BUDGET_FRACTION) as u32);
    if rescue_budget == 0 {
        return;
    }

    let min_score = compute_rescue_threshold(all_fragments, rel_scores, core_ids);
    if min_score == f64::INFINITY {
        return;
    }

    let selected_ids: FxHashSet<FragmentId> = selected.iter().map(|f| f.id.clone()).collect();
    let selected_paths: FxHashSet<Arc<str>> = selected.iter().map(|f| f.id.path.clone()).collect();
    let changed_paths: FxHashSet<Arc<str>> = core_ids.iter().map(|fid| fid.path.clone()).collect();

    let mut candidates: Vec<&Fragment> = all_fragments
        .iter()
        .filter(|f| {
            !selected_ids.contains(&f.id)
                && !core_ids.contains(&f.id)
                && !changed_paths.contains(&f.id.path)
                && !selected_paths.contains(&f.id.path)
                && rel_scores.get(&f.id).copied().unwrap_or(0.0) >= min_score
                && f.token_count <= rescue_budget
        })
        .collect();
    candidates.sort_by(|a, b| {
        let sa = rel_scores.get(&a.id).copied().unwrap_or(0.0);
        let sb = rel_scores.get(&b.id).copied().unwrap_or(0.0);
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut interval_idx = IntervalIndex::new();
    for f in selected.iter() {
        interval_idx.add(f);
    }

    let mut budget_used = 0u32;
    for cand in candidates {
        if budget_used + cand.token_count > rescue_budget {
            continue;
        }
        if interval_idx.overlaps(cand) {
            continue;
        }
        selected.push(cand.clone());
        interval_idx.add(cand);
        budget_used += cand.token_count;
    }
}

pub fn ensure_changed_files_represented(
    selected: &mut Vec<Fragment>,
    all_fragments: &[Fragment],
    changed_files: &[PathBuf],
    remaining_budget: u32,
    root_dir: &Path,
    preferred_revs: &[String],
    _batch_reader: Option<&mut CatFileBatch>,
) {
    let selected_paths: FxHashSet<String> = selected
        .iter()
        .map(|f| f.id.path.as_ref().to_string())
        .collect();
    let mut missing_paths: Vec<&PathBuf> = changed_files
        .iter()
        .filter(|p| !selected_paths.contains(&p.to_string_lossy().as_ref().to_string()))
        .collect();
    missing_paths.sort();

    if missing_paths.is_empty() {
        return;
    }

    let mut frags_by_path: HashMap<String, Vec<Fragment>> = HashMap::new();
    for f in all_fragments {
        let path_str = f.id.path.as_ref().to_string();
        if missing_paths
            .iter()
            .any(|p| p.to_string_lossy().as_ref() == path_str)
        {
            frags_by_path.entry(path_str).or_default().push(f.clone());
        }
    }

    let mut budget_left = remaining_budget;
    let mut selected_ids: FxHashSet<FragmentId> = selected.iter().map(|f| f.id.clone()).collect();
    let mut interval_idx = IntervalIndex::new();
    for f in selected.iter() {
        interval_idx.add(f);
    }

    for path in missing_paths.iter().copied() {
        let path_str = path.to_string_lossy().to_string();
        let candidates = frags_by_path.get(&path_str).cloned().unwrap_or_default();
        let candidates = if candidates.is_empty() {
            match create_whole_file_fragment(path, root_dir, preferred_revs, None) {
                Some(f) => vec![f],
                None => continue,
            }
        } else {
            candidates
        };

        if let Some(picked) = pick_smallest_fitting(&candidates, &selected_ids, budget_left) {
            if !interval_idx.overlaps(&picked) {
                budget_left = budget_left.saturating_sub(picked.token_count);
                selected_ids.insert(picked.id.clone());
                interval_idx.add(&picked);
                selected.push(picked);
            }
        }
    }
}
