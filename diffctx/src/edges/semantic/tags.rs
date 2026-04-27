use std::path::Path;

use rustc_hash::FxHashMap;

use crate::config::edge_weights::TAGS_SEMANTIC;
use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder};

pub struct TagsEdgeBuilder;

impl EdgeBuilder for TagsEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let mut ident_index: FxHashMap<&str, Vec<(&FragmentId, &str)>> = FxHashMap::default();

        for f in fragments {
            let path = f.path();
            for ident in &f.identifiers {
                if ident.len() >= TAGS_SEMANTIC.min_ident_len {
                    ident_index
                        .entry(ident.as_str())
                        .or_default()
                        .push((&f.id, path));
                }
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for (_, holders) in &ident_index {
            if holders.len() < 2 || holders.len() > TAGS_SEMANTIC.max_fragments_per_ident {
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
                        base::add_edge(
                            &mut edges,
                            src,
                            dst,
                            TAGS_SEMANTIC.weight,
                            TAGS_SEMANTIC.reverse_factor,
                        );
                    }
                }
            }
        }

        edges
    }
}
