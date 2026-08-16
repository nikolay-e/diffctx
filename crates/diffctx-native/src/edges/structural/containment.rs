use std::path::Path;

use rustc_hash::FxHashMap;

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{EdgeBuilder, add_edge};

pub struct ContainmentEdgeBuilder;

impl EdgeBuilder for ContainmentEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let weight = EDGE_WEIGHTS["containment"].forward;
        let reverse_factor = EDGE_WEIGHTS["containment"].reverse_factor;

        let mut by_path: FxHashMap<&str, Vec<&Fragment>> = FxHashMap::default();
        for f in fragments {
            by_path.entry(f.path()).or_default().push(f);
        }

        let mut edges: EdgeDict = FxHashMap::default();

        // Computed only for the experimental star below — OFF by default, and
        // measured net-negative when ON (#208): 21 corpus regressions vs 14
        // improvements, 2026-08-16 A/B at shipped defaults.
        let star_on = std::env::var_os("DIFFCTX_FILE_STAR").is_some();
        let reps = if star_on {
            super::super::base::file_representatives(fragments)
        } else {
            FxHashMap::default()
        };

        for (path, frags) in &by_path {
            if frags.len() < 2 {
                continue;
            }
            let mut sorted = frags.clone();
            sorted.sort_by(|a, b| {
                a.start_line()
                    .cmp(&b.start_line())
                    .then(b.end_line().cmp(&a.end_line()))
            });

            let mut stack: Vec<&Fragment> = Vec::new();

            for f in &sorted {
                while let Some(top) = stack.last() {
                    if f.start_line() > top.end_line() {
                        stack.pop();
                    } else {
                        break;
                    }
                }

                if let Some(parent) = stack.last() {
                    if parent.start_line() <= f.start_line()
                        && f.end_line() <= parent.end_line()
                        && parent.id != f.id
                    {
                        add_edge(&mut edges, &f.id, &parent.id, weight, reverse_factor);
                    }
                }

                stack.push(f);
            }

            // Experimental intra-file spine: connects every fragment to the
            // file representative so top-level siblings become mutually
            // reachable. Ships OFF — the 2026-08-16 corpus A/B measured it
            // net-negative (#208) — so by default a flat file's siblings are
            // reachable only through nesting and naming edges, and the
            // representative-only builders accept that.
            if star_on {
                if let Some(rep) = reps.get(*path) {
                    for f in &sorted {
                        if f.id != *rep {
                            add_edge(&mut edges, &f.id, rep, weight, reverse_factor);
                        }
                    }
                }
            }
        }

        edges
    }
}
