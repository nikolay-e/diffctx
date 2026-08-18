use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::selection::rescue;
use crate::fragmentation::create_whole_file_fragment;
use crate::git::CatFileBatch;
use crate::graph::Graph;
use crate::interval::IntervalIndex;
use crate::types::{Fragment, FragmentId, FragmentKind};

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
            let cat = graph.edge_category(&frag.id, nbr_id);
            if cat
                .map(|c| c != crate::graph::EdgeCategory::Semantic)
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
    let available: Vec<&&'a Fragment> = candidates
        .iter()
        .filter(|c| !selected_ids.contains(&c.id))
        .collect();
    let full = available.iter().find(|f| !f.kind.is_signature()).copied();
    let sig = available.iter().find(|f| f.kind.is_signature()).copied();
    full.or(sig).map(|f| *f)
}

fn change_coverage_rank(f: &Fragment, core_ids: &FxHashSet<FragmentId>) -> u8 {
    if core_ids.contains(&f.id) {
        return 0;
    }
    // An excerpt is cut from a core fragment around the diff hunk, so it always
    // covers the change; a signature only does when it belongs to a core.
    let is_core_stub = f.kind == FragmentKind::Excerpt
        || (f.kind.is_signature()
            && core_ids
                .iter()
                .any(|c| c.path == f.id.path && c.start_line == f.id.start_line));
    if is_core_stub { 1 } else { 2 }
}

fn pick_smallest_fitting(
    candidates: &[Fragment],
    selected_ids: &FxHashSet<FragmentId>,
    budget_left: u32,
    core_ids: &FxHashSet<FragmentId>,
) -> Option<Fragment> {
    let mut sorted: Vec<&Fragment> = candidates.iter().collect();
    // Prefer a fragment that actually covers the diff hunk (core_ids, i.e.
    // what render.rs marks `role: "changed"`), then its signature stub, over
    // an unrelated same-file fragment. Sorting by token_count alone picks
    // whichever candidate is smallest regardless of relevance, which can
    // silently hide the real change behind a tiny unrelated stub (#83).
    sorted.sort_by_key(|f| (change_coverage_rank(f, core_ids), f.token_count));
    for cand in &sorted {
        if cand.token_count == 0 || selected_ids.contains(&cand.id) {
            continue;
        }
        if cand.token_count <= budget_left {
            return Some((*cand).clone());
        }
    }
    // Nothing fits: the budget cap is a hard contract (cost(C) <= B). The
    // changed file stays unrepresented and shows up downstream as
    // changed-file retention < 1 rather than as a silent budget overrun.
    None
}

pub fn coherence_post_pass(
    selected: &mut Vec<Fragment>,
    all_fragments: &[Fragment],
    graph: &Graph,
    budget: u32,
    admissible_files: Option<&FxHashSet<Arc<str>>>,
) {
    let selected_ids: FxHashSet<FragmentId> = selected.iter().map(|f| f.id.clone()).collect();
    // #65/#211: the name lookup below can land on an arbitrary same-named
    // symbol in any file, so a pick opening a NEW file must be naming-
    // reachable like every other context.
    let open_paths: FxHashSet<Arc<str>> = selected.iter().map(|f| f.id.path.clone()).collect();
    let mut interval_idx = IntervalIndex::new();
    for f in selected.iter() {
        interval_idx.add(f);
    }
    let used: u32 = selected.iter().map(|f| f.token_count).sum();
    let mut remaining = budget.saturating_sub(used);

    let mut name_to_frags: FxHashMap<String, Vec<&Fragment>> = FxHashMap::default();
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
            && admissible_files
                .is_none_or(|a| open_paths.contains(&pick.id.path) || a.contains(&pick.id.path))
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
    context_scores.sort_by(|a, b| b.total_cmp(a));
    let idx = (context_scores.len() as f64 * (1.0 - rescue().min_score_percentile)) as usize;
    context_scores[idx.min(context_scores.len() - 1)]
}

pub fn rescue_nontrivial_context(
    selected: &mut Vec<Fragment>,
    all_fragments: &[Fragment],
    rel_scores: &FxHashMap<FragmentId, f64>,
    core_ids: &FxHashSet<FragmentId>,
    budget: u32,
    admissible_files: Option<&FxHashSet<Arc<str>>>,
) {
    let used: u32 = selected.iter().map(|f| f.token_count).sum();
    let remaining = budget.saturating_sub(used);
    let rescue_budget = remaining.min((budget as f64 * rescue().budget_fraction) as u32);
    if rescue_budget == 0 {
        return;
    }

    let min_score = compute_rescue_threshold(all_fragments, rel_scores, core_ids);
    if min_score == f64::INFINITY {
        return;
    }

    let selected_ids: FxHashSet<FragmentId> = selected.iter().map(|f| f.id.clone()).collect();
    // Files already represented anywhere in the selection are out of scope: the
    // metric this pass serves is file-level (gold files outside the diff), so
    // its budget only buys something when it reaches a *new* file.
    let mut represented_paths: FxHashSet<Arc<str>> =
        selected.iter().map(|f| f.id.path.clone()).collect();
    let changed_paths: FxHashSet<Arc<str>> = core_ids.iter().map(|fid| fid.path.clone()).collect();
    // The admission gate protects an already-useful selection from being
    // padded with reachable-but-wrong files. When the selection holds NO
    // context at all (changed files only), there is nothing to dilute and a
    // weak-channel or similarity-only candidate is strictly better than an
    // empty answer — measured: every gate-caused corpus regression was a
    // zero-context selection, while every gate win had context already.
    let has_context = selected.iter().any(|f| !changed_paths.contains(&f.id.path));
    let admissible_files = if has_context { admissible_files } else { None };

    let mut candidates: Vec<&Fragment> = all_fragments
        .iter()
        .filter(|f| {
            !selected_ids.contains(&f.id)
                && !core_ids.contains(&f.id)
                && !changed_paths.contains(&f.id.path)
                && !represented_paths.contains(&f.id.path)
                && rel_scores.get(&f.id).copied().unwrap_or(0.0) >= min_score
                && f.token_count <= rescue_budget
                // #65/#211: every pick here opens a new file by construction,
                // so the admission gate applies with no open-file exemption.
                && admissible_files.is_none_or(|a| a.contains(&f.id.path))
        })
        .collect();
    candidates.sort_by(|a, b| {
        let sa = rel_scores.get(&a.id).copied().unwrap_or(0.0);
        let sb = rel_scores.get(&b.id).copied().unwrap_or(0.0);
        sb.total_cmp(&sa).then_with(|| a.id.cmp(&b.id))
    });

    let mut interval_idx = IntervalIndex::new();
    for f in selected.iter() {
        interval_idx.add(f);
    }

    let mut budget_used = 0u32;
    for cand in candidates {
        // The path filter above was a snapshot of the incoming selection, so
        // without this the pass could spend its whole budget stacking several
        // fragments of one newly reached file — no gain on the file-level
        // metric it exists for, at the cost of every other file it could
        // still have reached.
        if represented_paths.contains(&cand.id.path) {
            continue;
        }
        if budget_used + cand.token_count > rescue_budget {
            continue;
        }
        if interval_idx.overlaps(cand) {
            continue;
        }
        selected.push(cand.clone());
        interval_idx.add(cand);
        represented_paths.insert(cand.id.path.clone());
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
    mut batch_reader: Option<&mut CatFileBatch>,
    core_ids: &FxHashSet<FragmentId>,
    core_excerpts: &FxHashMap<FragmentId, Fragment>,
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

    // Membership through a set, not a scan of `missing_paths` per fragment.
    // The scan ran `to_string_lossy().to_string()` on both sides of every
    // comparison, so grouping cost fragments x missing-paths *string
    // allocations* — on a range that adds hundreds of files it dominated the
    // whole run. Same buckets, same first-seen order within each.
    let missing_lookup: FxHashSet<String> = missing_paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let mut frags_by_path: FxHashMap<String, Vec<Fragment>> = FxHashMap::default();
    for f in all_fragments.iter().chain(core_excerpts.values()) {
        let path_str = f.id.path.as_ref();
        if missing_lookup.contains(path_str) {
            frags_by_path
                .entry(path_str.to_string())
                .or_default()
                .push(f.clone());
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
            match create_whole_file_fragment(
                path,
                root_dir,
                preferred_revs,
                batch_reader.as_deref_mut(),
            ) {
                Some(f) => vec![f],
                None => continue,
            }
        } else {
            candidates
        };

        if let Some(picked) =
            pick_smallest_fitting(&candidates, &selected_ids, budget_left, core_ids)
        {
            if !interval_idx.overlaps(&picked) {
                budget_left = budget_left.saturating_sub(picked.token_count);
                selected_ids.insert(picked.id.clone());
                interval_idx.add(&picked);
                selected.push(picked);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frag(
        path: &str,
        start: u32,
        end: u32,
        kind: crate::types::FragmentKind,
        tokens: u32,
    ) -> Fragment {
        Fragment {
            id: FragmentId::new(Arc::from(path), start, end),
            kind,
            content: Arc::from(format!("fragment {path}:{start}-{end}")),
            identifiers: FxHashSet::default(),
            token_count: tokens,
            symbol_name: None,
        }
    }

    /// Regression for #83: when a changed file has no selected fragment and
    /// the postpass fallback must pick one, a same-file signature stub that
    /// is merely *smaller* must not be preferred over a same-file fragment
    /// that actually covers the diff hunk (core_ids), as long as the core
    /// fragment also fits the remaining budget.
    #[test]
    fn ensure_changed_files_represented_prefers_core_fragment_when_it_fits() {
        let core = frag("a.ts", 10, 20, crate::types::FragmentKind::Function, 60);
        let stub = frag(
            "a.ts",
            10,
            10,
            crate::types::FragmentKind::FunctionSignature,
            10,
        );
        let all_fragments = vec![core.clone(), stub.clone()];
        let core_ids: FxHashSet<FragmentId> = std::iter::once(core.id.clone()).collect();
        let changed_files = vec![PathBuf::from("a.ts")];
        let mut selected: Vec<Fragment> = Vec::new();

        ensure_changed_files_represented(
            &mut selected,
            &all_fragments,
            &changed_files,
            100,
            Path::new("."),
            &[],
            None,
            &core_ids,
            &FxHashMap::default(),
        );

        assert_eq!(selected.len(), 1, "expected exactly one fallback fragment");
        assert_eq!(
            selected[0].id, core.id,
            "fallback picked the signature stub instead of the fragment covering the actual diff hunk"
        );
    }

    /// When the core fragment does NOT fit the remaining budget, falling
    /// back to the smaller non-core stub is still the correct behavior
    /// (some representation beats none).
    #[test]
    fn ensure_changed_files_represented_falls_back_to_stub_when_core_does_not_fit() {
        let core = frag("a.ts", 10, 20, crate::types::FragmentKind::Function, 60);
        let stub = frag(
            "a.ts",
            10,
            10,
            crate::types::FragmentKind::FunctionSignature,
            10,
        );
        let all_fragments = vec![core.clone(), stub.clone()];
        let core_ids: FxHashSet<FragmentId> = std::iter::once(core.id.clone()).collect();
        let changed_files = vec![PathBuf::from("a.ts")];
        let mut selected: Vec<Fragment> = Vec::new();

        ensure_changed_files_represented(
            &mut selected,
            &all_fragments,
            &changed_files,
            15,
            Path::new("."),
            &[],
            None,
            &core_ids,
            &FxHashMap::default(),
        );

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, stub.id);
    }

    fn cost(selected: &[Fragment]) -> u32 {
        selected.iter().map(|f| f.token_count).sum()
    }

    /// `cost(C) <= B` is stated as a hard contract in `pick_smallest_fitting`
    /// and gated at five separate call sites, none of which was asserted. The
    /// oracle corpus cannot catch a breach either: its budget is always >=2.5x
    /// the whole repository, so no post-pass ever runs near the cap there.
    #[test]
    fn post_passes_never_push_the_selection_past_the_budget() {
        use crate::types::FragmentKind;

        let core = frag("changed.rs", 1, 9, FragmentKind::Function, 40);
        let all = vec![
            core.clone(),
            frag("changed.rs", 20, 60, FragmentKind::Function, 300),
            frag("changed.rs", 70, 75, FragmentKind::FunctionSignature, 12),
            frag("other.rs", 1, 30, FragmentKind::Class, 180),
            frag("other.rs", 40, 44, FragmentKind::Function, 25),
        ];
        let core_ids: FxHashSet<FragmentId> = std::iter::once(core.id.clone()).collect();
        let rel: FxHashMap<FragmentId, f64> = all.iter().map(|f| (f.id.clone(), 0.6)).collect();
        let changed = vec![PathBuf::from("changed.rs"), PathBuf::from("other.rs")];
        let excerpts: FxHashMap<FragmentId, Fragment> = FxHashMap::default();

        for budget in [0u32, 11, 12, 40, 65, 200, 600] {
            let mut selected: Vec<Fragment> = if budget >= core.token_count {
                vec![core.clone()]
            } else {
                Vec::new()
            };

            rescue_nontrivial_context(&mut selected, &all, &rel, &core_ids, budget, None);
            assert!(
                cost(&selected) <= budget,
                "rescue overran budget {budget}: cost {}",
                cost(&selected)
            );

            let remaining = budget.saturating_sub(cost(&selected));
            ensure_changed_files_represented(
                &mut selected,
                &all,
                &changed,
                remaining,
                Path::new("."),
                &[],
                None,
                &core_ids,
                &excerpts,
            );
            assert!(
                cost(&selected) <= budget,
                "ensure_changed_files_represented overran budget {budget}: cost {}",
                cost(&selected)
            );

            let ids: FxHashSet<&FragmentId> = selected.iter().map(|f| &f.id).collect();
            assert_eq!(ids.len(), selected.len(), "a fragment was selected twice");
        }
    }

    /// The rescue budget exists to reach files the selection missed entirely,
    /// which is what `nontrivial_file_recall` counts. The path filter was a
    /// snapshot taken before the loop, so several fragments of one freshly
    /// reached file could absorb the whole allowance.
    #[test]
    fn rescue_spends_its_budget_on_distinct_files() {
        use crate::types::FragmentKind;

        let core = frag("changed.rs", 1, 10, FragmentKind::Function, 100);
        let mut all = vec![core.clone()];
        let mut rel: FxHashMap<FragmentId, f64> = FxHashMap::default();
        rel.insert(core.id.clone(), 1.0);

        // Two fragments in one unrelated file scoring above a third in another
        // file, so the crowded file is visited first. At 40 tokens each only two
        // of the three fit the allowance — the budget, not the threshold, is
        // what decides whether the second file is ever reached.
        for i in 0..2u32 {
            let f = frag(
                "crowded.rs",
                1 + i * 100,
                50 + i * 100,
                FragmentKind::Function,
                40,
            );
            rel.insert(f.id.clone(), 0.90 - f64::from(i) * 0.01);
            all.push(f);
        }
        let lonely = frag("lonely.rs", 1, 50, FragmentKind::Function, 40);
        rel.insert(lonely.id.clone(), 0.88);
        all.push(lonely);

        // Low-score filler so the 80th-percentile threshold admits exactly the
        // three fragments above instead of collapsing onto the maximum.
        for i in 0..12u32 {
            let f = frag(
                "filler.rs",
                1 + i * 100,
                50 + i * 100,
                FragmentKind::Function,
                40,
            );
            rel.insert(f.id.clone(), 0.10);
            all.push(f);
        }

        let core_ids: FxHashSet<FragmentId> = std::iter::once(core.id.clone()).collect();

        let mut selected = vec![core];
        // 5% of 2000 = 100 tokens of rescue: room for two 40-token picks.
        rescue_nontrivial_context(&mut selected, &all, &rel, &core_ids, 2_000, None);

        let rescued: Vec<&str> = selected
            .iter()
            .skip(1)
            .map(|f| f.id.path.as_ref())
            .collect();
        let distinct: FxHashSet<&str> = rescued.iter().copied().collect();
        assert_eq!(
            rescued.len(),
            distinct.len(),
            "rescue stacked several fragments of one file: {rescued:?}"
        );
        assert!(
            distinct.contains("lonely.rs"),
            "the second file was never reached: {rescued:?}"
        );
    }

    #[test]
    fn pick_smallest_fitting_refuses_every_oversized_candidate() {
        use crate::types::FragmentKind;

        let candidates = vec![
            frag("a.rs", 1, 40, FragmentKind::Function, 500),
            frag("a.rs", 50, 90, FragmentKind::Function, 400),
        ];
        let core_ids: FxHashSet<FragmentId> = FxHashSet::default();
        assert!(
            pick_smallest_fitting(&candidates, &FxHashSet::default(), 399, &core_ids).is_none(),
            "returned a candidate that does not fit — the budget contract is broken"
        );
        assert!(
            pick_smallest_fitting(&candidates, &FxHashSet::default(), 400, &core_ids).is_some(),
            "refused a candidate that fits exactly"
        );
    }

    #[test]
    fn pick_smallest_fitting_skips_already_selected_and_zero_cost_fragments() {
        use crate::types::FragmentKind;

        let taken = frag("a.rs", 1, 10, FragmentKind::Function, 30);
        let zero = frag("a.rs", 20, 30, FragmentKind::Function, 0);
        let free = frag("a.rs", 40, 50, FragmentKind::Function, 60);
        let selected: FxHashSet<FragmentId> = std::iter::once(taken.id.clone()).collect();
        let picked = pick_smallest_fitting(
            &[taken, zero, free.clone()],
            &selected,
            1_000,
            &FxHashSet::default(),
        );
        assert_eq!(picked.map(|f| f.id), Some(free.id));
    }
}
