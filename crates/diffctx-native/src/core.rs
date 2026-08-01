use rustc_hash::{FxHashMap, FxHashSet};

use crate::types::{DiffHunk, Fragment, FragmentId, FragmentKind};

fn kind_priority(kind: FragmentKind) -> u8 {
    if kind.is_semantic() { 0 } else { 1 }
}

fn find_core_for_hunk(frags: &[&Fragment], h_start: u32, h_end: u32) -> FxHashSet<FragmentId> {
    let mut core = FxHashSet::default();

    let covering: Vec<&Fragment> = frags
        .iter()
        .copied()
        .filter(|f| f.start_line() <= h_start && h_end <= f.end_line())
        .collect();
    // Every tie-break here ends on the fragment id. Kind class and span length
    // leave genuine ties (nested or overlapping same-size definitions), and
    // without a final total order the seed was decided by the order the parser
    // emitted fragments in — so which code the output marks as changed shifted
    // for reasons unrelated to the diff.
    if !covering.is_empty() {
        let best = covering
            .iter()
            .min_by(|a, b| {
                let ka = kind_priority(a.kind);
                let kb = kind_priority(b.kind);
                ka.cmp(&kb)
                    .then(a.line_count().cmp(&b.line_count()))
                    .then_with(|| a.id.cmp(&b.id))
            })
            .unwrap();
        core.insert(best.id.clone());
        return core;
    }

    let overlapping: Vec<&Fragment> = frags
        .iter()
        .copied()
        .filter(|f| f.start_line() <= h_end && f.end_line() >= h_start)
        .collect();
    if !overlapping.is_empty() {
        for f in &overlapping {
            core.insert(f.id.clone());
        }
        return core;
    }

    let before: Vec<&Fragment> = frags
        .iter()
        .copied()
        .filter(|f| f.end_line() < h_start)
        .collect();
    let after: Vec<&Fragment> = frags
        .iter()
        .copied()
        .filter(|f| f.start_line() > h_end)
        .collect();
    if let Some(nearest_before) = before.iter().max_by(|a, b| {
        a.end_line()
            .cmp(&b.end_line())
            .then_with(|| a.id.cmp(&b.id))
    }) {
        core.insert(nearest_before.id.clone());
    }
    if let Some(nearest_after) = after.iter().min_by(|a, b| {
        a.start_line()
            .cmp(&b.start_line())
            .then_with(|| a.id.cmp(&b.id))
    }) {
        core.insert(nearest_after.id.clone());
    }

    core
}

fn add_container_headers(
    core_ids: &mut FxHashSet<FragmentId>,
    frags_by_path: &FxHashMap<&str, Vec<&Fragment>>,
) {
    let core_paths: FxHashSet<&str> = core_ids.iter().map(|fid| fid.path.as_ref()).collect();
    let mut headers_to_add = Vec::new();

    for &path in &core_paths {
        if let Some(frags) = frags_by_path.get(path) {
            for frag in frags {
                if !frag.kind.is_container() || core_ids.contains(&frag.id) {
                    continue;
                }
                let contains_core = core_ids.iter().any(|core_id| {
                    core_id.path.as_ref() == path
                        && frag.start_line() <= core_id.start_line
                        && core_id.end_line <= frag.end_line()
                });
                if contains_core {
                    headers_to_add.push(frag.id.clone());
                }
            }
        }
    }

    for h in headers_to_add {
        core_ids.insert(h);
    }
}

pub fn identify_core_fragments(
    hunks: &[DiffHunk],
    all_fragments: &[Fragment],
) -> FxHashSet<FragmentId> {
    let mut frags_by_path: FxHashMap<&str, Vec<&Fragment>> = FxHashMap::default();
    for frag in all_fragments {
        frags_by_path.entry(frag.path()).or_default().push(frag);
    }

    let mut core_ids = FxHashSet::default();
    for h in hunks {
        if let Some(frags) = frags_by_path.get(h.path.as_ref()) {
            let (h_start, h_end) = h.core_selection_range();
            core_ids.extend(find_core_for_hunk(frags, h_start, h_end));
        }
    }

    add_container_headers(&mut core_ids, &frags_by_path);
    core_ids
}

fn map_hunks_to_fragments(
    hunks: &[DiffHunk],
    core_ids: &FxHashSet<FragmentId>,
    all_fragments: &[Fragment],
) -> FxHashMap<FragmentId, f64> {
    let mut result: FxHashMap<FragmentId, f64> = FxHashMap::default();
    for h in hunks {
        let (h_start, h_end) = h.core_selection_range();
        let hunk_size = (h_end as i64 - h_start as i64 + 1).max(1) as f64;
        for frag in all_fragments {
            if !core_ids.contains(&frag.id) || frag.path() != h.path.as_ref() {
                continue;
            }
            if frag.start_line() <= h_end && frag.end_line() >= h_start {
                *result.entry(frag.id.clone()).or_insert(0.0) += hunk_size;
            }
        }
    }
    result
}

fn add_container_weights(
    frag_hunk_lines: &mut FxHashMap<FragmentId, f64>,
    core_ids: &FxHashSet<FragmentId>,
    all_fragments: &[Fragment],
) {
    let mut to_add = Vec::new();
    for frag in all_fragments {
        if !core_ids.contains(&frag.id) || frag_hunk_lines.contains_key(&frag.id) {
            continue;
        }
        if !frag.kind.is_container() {
            continue;
        }
        let contained_weight: f64 = frag_hunk_lines
            .iter()
            .filter(|(fid, _)| {
                fid.path.as_ref() == frag.path()
                    && frag.start_line() <= fid.start_line
                    && fid.end_line <= frag.end_line()
            })
            .map(|(_, w)| *w)
            .sum();
        if contained_weight > 0.0 {
            to_add.push((frag.id.clone(), contained_weight));
        }
    }
    for (id, w) in to_add {
        frag_hunk_lines.insert(id, w);
    }
}

fn best_hunk_size_for_path(hunks: &[DiffHunk], path: &str) -> u32 {
    let mut best = 0u32;
    for h in hunks {
        if h.path.as_ref() == path {
            let (h_start, h_end) = h.core_selection_range();
            let size = h_end.saturating_sub(h_start) + 1;
            best = best.max(size);
        }
    }
    best
}

fn fill_missing_core_weights(
    frag_hunk_lines: &mut FxHashMap<FragmentId, f64>,
    core_ids: &FxHashSet<FragmentId>,
    hunks: &[DiffHunk],
) {
    let missing: Vec<FragmentId> = core_ids
        .iter()
        .filter(|fid| !frag_hunk_lines.contains_key(*fid))
        .cloned()
        .collect();
    for fid in missing {
        let best = best_hunk_size_for_path(hunks, fid.path.as_ref());
        if best > 0 {
            frag_hunk_lines.insert(fid, best as f64);
        }
    }
}

pub fn compute_seed_weights(
    hunks: &[DiffHunk],
    core_ids: &FxHashSet<FragmentId>,
    all_fragments: &[Fragment],
) -> FxHashMap<FragmentId, f64> {
    let mut frag_hunk_lines = map_hunks_to_fragments(hunks, core_ids, all_fragments);
    if frag_hunk_lines.is_empty() {
        return FxHashMap::default();
    }

    add_container_weights(&mut frag_hunk_lines, core_ids, all_fragments);
    fill_missing_core_weights(&mut frag_hunk_lines, core_ids, hunks);

    frag_hunk_lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn frag(path: &str, start: u32, end: u32, kind: FragmentKind) -> Fragment {
        Fragment {
            id: FragmentId::new(Arc::from(path), start, end),
            kind,
            content: Arc::from(""),
            identifiers: FxHashSet::default(),
            token_count: 10,
            symbol_name: None,
        }
    }

    fn hunk(path: &str, start: u32, len: u32) -> DiffHunk {
        DiffHunk {
            path: Arc::from(path),
            new_start: start,
            new_len: len,
            old_start: start,
            old_len: len,
        }
    }

    fn sorted_ids(core: &FxHashSet<FragmentId>) -> Vec<FragmentId> {
        let mut v: Vec<FragmentId> = core.iter().cloned().collect();
        v.sort();
        v
    }

    /// Which fragment a hunk seeds decides what the output marks as changed.
    /// The tie-breaks in `find_core_for_hunk` compare kind class and span
    /// length only, so equally-ranked candidates were resolved by whatever
    /// order the parser happened to emit them in — the same order dependence
    /// already ruled out for `drop_redundant_signatures` and
    /// `cap_context_fragments`.
    #[test]
    fn core_identification_is_invariant_under_fragment_order() {
        // Two equally-sized covering candidates, plus an equally-distant
        // neighbour on each side of a second hunk that nothing covers.
        let fragments = vec![
            frag("a.rs", 10, 20, FragmentKind::Function),
            frag("a.rs", 15, 25, FragmentKind::Function),
            frag("a.rs", 60, 70, FragmentKind::Function),
            frag("a.rs", 90, 100, FragmentKind::Function),
            frag("b.rs", 1, 11, FragmentKind::Function),
            frag("b.rs", 30, 40, FragmentKind::Function),
        ];
        let hunks = vec![
            hunk("a.rs", 16, 3),
            hunk("a.rs", 80, 1),
            hunk("b.rs", 20, 1),
        ];

        let baseline = sorted_ids(&identify_core_fragments(&hunks, &fragments));
        assert!(!baseline.is_empty(), "no core identified at all");

        for permuted in [
            {
                let mut v = fragments.clone();
                v.reverse();
                v
            },
            {
                let mut v = fragments.clone();
                v.rotate_left(3);
                v
            },
            {
                let mut v = fragments.clone();
                v.sort_by_key(|f| std::cmp::Reverse(f.start_line()));
                v
            },
        ] {
            assert_eq!(
                sorted_ids(&identify_core_fragments(&hunks, &permuted)),
                baseline,
                "core set changed with fragment order"
            );
        }
    }
}
