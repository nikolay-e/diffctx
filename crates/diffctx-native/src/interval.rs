use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::types::{Fragment, FragmentId};

pub struct IntervalIndex {
    by_path: FxHashMap<Arc<str>, Vec<(u32, u32)>>,
    ids: FxHashSet<FragmentId>,
}

impl IntervalIndex {
    pub fn new() -> Self {
        Self {
            by_path: FxHashMap::default(),
            ids: FxHashSet::default(),
        }
    }

    pub fn add(&mut self, frag: &Fragment) {
        self.add_id(&frag.id);
    }

    pub fn add_id(&mut self, frag_id: &FragmentId) {
        self.ids.insert(frag_id.clone());
        let intervals = self.by_path.entry(frag_id.path.clone()).or_default();
        let item = (frag_id.start_line, frag_id.end_line);
        let pos = intervals.binary_search(&item).unwrap_or_else(|e| e);
        intervals.insert(pos, item);
    }

    pub fn contains(&self, frag_id: &FragmentId) -> bool {
        self.ids.contains(frag_id)
    }

    pub fn overlaps(&self, frag: &Fragment) -> bool {
        let intervals = match self.by_path.get(&frag.id.path) {
            Some(v) => v,
            None => return false,
        };
        let upper = intervals.partition_point(|&(s, _)| s <= frag.end_line());
        for i in 0..upper {
            let (start, end) = intervals[i];
            if start == frag.start_line() && end == frag.end_line() {
                continue;
            }
            // Strict `>`: a fragment starting on the very last line of an
            // already-selected fragment shares exactly one boundary line. We
            // deliberately tolerate that one-line overlap rather than drop the
            // candidate, because compact languages (Rust/Go/Scala one-liners,
            // Lisp `}{` chains) routinely produce back-to-back fragments sharing
            // that boundary line; rejecting them would silently discard the
            // next fragment's unique content for the sake of one duplicated line.
            //
            // KNOWN ASYMMETRY, pinned by
            // `overlaps_tolerates_a_shared_boundary_in_one_direction_only`.
            // `partition_point` bounds the scan by `start <= candidate.end`
            // (non-strict) while this comparison is strict, so the tolerance
            // applies in one direction only: selected [1,10] vs candidate
            // [10,20] is kept, but the mirrored selected [10,20] vs candidate
            // [1,10] is dropped. Which side a fragment lands on depends on
            // greedy visit order, not on relevance. Adding the mirrored strict
            // bound (`start < frag.end_line() &&`) makes it symmetric and is
            // Q-class: on the 2725-case corpus it moves 3 cases above threshold
            // (javascript_059, rust_008, rust_027) and 3 below
            // (frontend_010, r_lang_006, r_lang_009) — net zero, so it belongs
            // to a calibration cycle boundary, not to an incidental change.
            if end > frag.start_line() {
                return true;
            }
        }
        false
    }

    pub fn is_superset_of(&self, frag: &Fragment) -> bool {
        let intervals = match self.by_path.get(&frag.id.path) {
            Some(v) => v,
            None => return false,
        };
        let upper = intervals.partition_point(|&(s, _)| s <= frag.start_line());
        for i in 0..upper {
            let (start, end) = intervals[i];
            if start == frag.start_line() && end == frag.end_line() {
                continue;
            }
            if start <= frag.start_line() && frag.end_line() <= end {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FragmentKind;

    fn frag(path: &str, start: u32, end: u32) -> Fragment {
        Fragment {
            id: FragmentId::new(Arc::from(path), start, end),
            kind: FragmentKind::Function,
            content: Arc::from(""),
            identifiers: FxHashSet::default(),
            token_count: 1,
            symbol_name: None,
        }
    }

    fn index_with(spans: &[(u32, u32)]) -> IntervalIndex {
        let mut idx = IntervalIndex::new();
        for &(s, e) in spans {
            idx.add(&frag("a.rs", s, e));
        }
        idx
    }

    #[test]
    fn overlaps_is_true_for_genuine_intersections() {
        let idx = index_with(&[(10, 20)]);
        for &(s, e) in &[(15, 25), (11, 19), (9, 21), (5, 15), (10, 20 + 1)] {
            assert!(
                idx.overlaps(&frag("a.rs", s, e)),
                "[{s},{e}] should intersect [10,20]"
            );
        }
    }

    /// Pins the known asymmetry documented on the comparison in `overlaps`.
    /// The forward direction is the deliberate boundary tolerance; the mirrored
    /// direction drops the candidate for the same geometry, so the verdict
    /// depends on greedy visit order. Making it symmetric is Q-class (see the
    /// comment for the measured corpus effect) — this test exists so the
    /// current behaviour cannot change silently, in either direction.
    #[test]
    fn overlaps_tolerates_a_shared_boundary_in_one_direction_only() {
        assert!(
            !index_with(&[(1, 10)]).overlaps(&frag("a.rs", 10, 20)),
            "candidate starting on the selected fragment's last line must be tolerated"
        );
        assert!(
            index_with(&[(10, 20)]).overlaps(&frag("a.rs", 1, 10)),
            "the mirrored case is currently reported as overlapping; if this now \
             passes as tolerated, the symmetry fix landed — update the corpus baseline"
        );
    }

    #[test]
    fn overlaps_verdicts_are_pinned_across_the_boundary_matrix() {
        // (selected, candidate) -> expected verdict. Encodes the asymmetry
        // above rather than assuming symmetry, so any change to either
        // comparison shows up here as a concrete diff.
        let expected = [
            ((1, 10), (10, 20), false),
            ((10, 20), (1, 10), true),
            ((10, 20), (20, 30), false),
            ((20, 30), (10, 20), true),
            ((10, 20), (11, 19), true),
            ((11, 19), (10, 20), true),
            ((10, 20), (9, 21), true),
            ((9, 21), (10, 20), true),
            // Another face of the same asymmetry: a one-line fragment at the
            // selected span's start does not block it, but the reverse does.
            ((10, 20), (10, 10), true),
            ((10, 10), (10, 20), false),
        ];
        for (selected, candidate, want) in expected {
            let got = index_with(&[selected]).overlaps(&frag("a.rs", candidate.0, candidate.1));
            assert_eq!(
                got, want,
                "selected {selected:?} vs candidate {candidate:?}: got {got}, want {want}"
            );
        }
    }

    #[test]
    fn overlaps_ignores_other_paths() {
        let idx = index_with(&[(10, 20)]);
        assert!(!idx.overlaps(&frag("b.rs", 15, 16)));
    }

    #[test]
    fn identical_span_is_reported_by_contains_not_by_overlaps() {
        // The exact-span `continue` means a duplicate is NOT an overlap, so
        // callers must rely on `contains` to avoid charging the budget twice.
        let idx = index_with(&[(10, 20)]);
        let same = frag("a.rs", 10, 20);
        assert!(!idx.overlaps(&same));
        assert!(idx.contains(&same.id));
    }

    #[test]
    fn is_superset_of_detects_enclosure_and_ignores_identical_spans() {
        let idx = index_with(&[(10, 30)]);
        assert!(idx.is_superset_of(&frag("a.rs", 15, 25)));
        assert!(idx.is_superset_of(&frag("a.rs", 10, 25)));
        assert!(!idx.is_superset_of(&frag("a.rs", 10, 30)));
        assert!(!idx.is_superset_of(&frag("a.rs", 5, 25)));
        assert!(!idx.is_superset_of(&frag("a.rs", 25, 35)));
    }

    #[test]
    fn add_keeps_intervals_sorted_regardless_of_insertion_order() {
        let forward = index_with(&[(1, 5), (10, 20), (30, 40)]);
        let shuffled = index_with(&[(30, 40), (1, 5), (10, 20)]);
        let path: Arc<str> = Arc::from("a.rs");
        assert_eq!(forward.by_path[&path], shuffled.by_path[&path]);
        assert!(forward.by_path[&path].windows(2).all(|w| w[0] <= w[1]));
    }
}
