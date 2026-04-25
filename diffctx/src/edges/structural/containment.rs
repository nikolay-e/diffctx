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

        for (_path, frags) in &by_path {
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
        }

        edges
    }
}
