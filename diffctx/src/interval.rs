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
            // already-selected fragment is adjacent, not overlapping. Compact
            // languages (Rust/Go/Scala one-liners, Lisp `}{` chains) routinely
            // produce back-to-back fragments sharing exactly that boundary
            // line; treating it as overlap silently drops the next fragment.
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
