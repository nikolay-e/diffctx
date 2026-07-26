use std::path::Path;

use rustc_hash::FxHashMap;

use crate::config::limits::SIBLING;
use crate::config::weights::EDGE_WEIGHTS;
use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{EdgeBuilder, add_edge};

pub struct SiblingEdgeBuilder;

impl SiblingEdgeBuilder {
    fn group_files_by_dir<'a>(&self, fragments: &'a [Fragment]) -> FxHashMap<String, Vec<&'a str>> {
        let mut by_dir: FxHashMap<String, Vec<&str>> = FxHashMap::default();
        for f in fragments {
            let path = Path::new(f.path());
            let dir = path
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let path_str = f.path();
            let files = by_dir.entry(dir).or_default();
            if !files.contains(&path_str) {
                files.push(path_str);
            }
        }
        by_dir
    }

    fn build_file_representative_map(
        &self,
        fragments: &[Fragment],
    ) -> FxHashMap<String, FragmentId> {
        let mut file_to_rep: FxHashMap<String, FragmentId> = FxHashMap::default();
        let mut file_to_token_count: FxHashMap<String, u32> = FxHashMap::default();

        for f in fragments {
            let path = f.path().to_string();
            let existing_count = file_to_token_count.get(&path).copied().unwrap_or(0);
            if !file_to_rep.contains_key(&path) || f.token_count > existing_count {
                file_to_rep.insert(path.clone(), f.id.clone());
                file_to_token_count.insert(path, f.token_count);
            }
        }

        file_to_rep
    }
}

impl EdgeBuilder for SiblingEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let weight = EDGE_WEIGHTS["sibling"].forward;
        let reverse_factor = EDGE_WEIGHTS["sibling"].reverse_factor;

        let by_dir = self.group_files_by_dir(fragments);
        let file_to_rep = self.build_file_representative_map(fragments);

        let mut edges: EdgeDict = FxHashMap::default();

        for (_dir, files) in &by_dir {
            let mut file_list: Vec<&str> = files.clone();
            file_list.sort_unstable();
            if file_list.len() > SIBLING.max_files_per_dir {
                file_list.truncate(SIBLING.max_files_per_dir);
            }
            if file_list.len() < 2 {
                continue;
            }

            for i in 0..file_list.len() {
                for j in (i + 1)..file_list.len() {
                    if let (Some(f1_id), Some(f2_id)) =
                        (file_to_rep.get(file_list[i]), file_to_rep.get(file_list[j]))
                    {
                        add_edge(&mut edges, f1_id, f2_id, weight, reverse_factor);
                    }
                }
            }
        }

        edges
    }

    fn category_label(&self) -> Option<&str> {
        Some("sibling")
    }
}
