use std::path::Path;

use rustc_hash::FxHashMap;

use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder};

const WEIGHT: f64 = 0.30;
const REVERSE_FACTOR: f64 = 0.70;
const MAX_FRAGMENTS_PER_IDENT: usize = 5;
const MIN_IDENT_LEN: usize = 3;

pub struct TagsEdgeBuilder;

impl EdgeBuilder for TagsEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let mut ident_index: FxHashMap<&str, Vec<(&FragmentId, &str)>> = FxHashMap::default();

        for f in fragments {
            let path = f.path();
            for ident in &f.identifiers {
                if ident.len() >= MIN_IDENT_LEN {
                    ident_index
                        .entry(ident.as_str())
                        .or_default()
                        .push((&f.id, path));
                }
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for (_, holders) in &ident_index {
            if holders.len() < 2 || holders.len() > MAX_FRAGMENTS_PER_IDENT {
                continue;
            }

            let mut cross_file_groups: FxHashMap<&str, Vec<&FragmentId>> = FxHashMap::default();
            for (fid, path) in holders {
                cross_file_groups.entry(path).or_default().push(fid);
            }

            if cross_file_groups.len() < 2 {
                continue;
            }

            let all_ids: Vec<&FragmentId> = holders.iter().map(|(fid, _)| *fid).collect();
            for i in 0..all_ids.len() {
                for j in (i + 1)..all_ids.len() {
                    let src = all_ids[i];
                    let dst = all_ids[j];
                    if src.path != dst.path {
                        base::add_edge(&mut edges, src, dst, WEIGHT, REVERSE_FACTOR);
                    }
                }
            }
        }

        edges
    }
}
