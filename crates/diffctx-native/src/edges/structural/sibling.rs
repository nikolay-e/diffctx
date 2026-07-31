use std::path::Path;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::limits::SIBLING;
use crate::config::weights::EDGE_WEIGHTS;
use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{EdgeBuilder, add_edge};

pub struct SiblingEdgeBuilder;

impl SiblingEdgeBuilder {
    fn group_files_by_dir<'a>(&self, fragments: &'a [Fragment]) -> FxHashMap<String, Vec<&'a str>> {
        let mut by_dir: FxHashMap<String, Vec<&str>> = FxHashMap::default();
        // A path belongs to exactly one directory, so one set of already-seen
        // paths is enough to dedupe every bucket. This used to be
        // `Vec::contains` against the bucket, i.e. a scan of the directory's
        // whole file list per fragment — quadratic in exactly the shape this
        // builder exists for (thousands of files in one directory), while
        // producing the same buckets in the same first-seen order.
        let mut seen: FxHashSet<&str> = FxHashSet::default();
        for f in fragments {
            let path_str = f.path();
            if !seen.insert(path_str) {
                continue;
            }
            let dir = Path::new(path_str)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            by_dir.entry(dir).or_default().push(path_str);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FragmentKind;
    use rustc_hash::FxHashSet as Set;
    use std::sync::Arc;

    fn frag(path: &str, start: u32) -> Fragment {
        Fragment {
            id: crate::types::FragmentId::new(Arc::from(path), start, start + 5),
            kind: FragmentKind::Function,
            content: Arc::from(""),
            identifiers: Set::default(),
            token_count: 10,
            symbol_name: None,
        }
    }

    /// Grouping deduped files with `Vec::contains` against the bucket, so every
    /// fragment scanned its directory's whole file list. The buckets it produces
    /// are what matters: one entry per file, in first-seen order, regardless of
    /// how many fragments each file contributes.
    #[test]
    fn each_file_appears_once_per_directory_in_first_seen_order() {
        // Several fragments per file, files interleaved across two directories.
        let fragments = vec![
            frag("src/b.rs", 1),
            frag("src/a.rs", 1),
            frag("src/b.rs", 20),
            frag("lib/c.rs", 1),
            frag("src/a.rs", 40),
            frag("lib/c.rs", 30),
        ];

        let by_dir = SiblingEdgeBuilder.group_files_by_dir(&fragments);

        assert_eq!(
            by_dir.get("src").map(Vec::as_slice),
            Some(&["src/b.rs", "src/a.rs"][..])
        );
        assert_eq!(
            by_dir.get("lib").map(Vec::as_slice),
            Some(&["lib/c.rs"][..])
        );
    }

    /// The per-directory pair loop is quadratic by nature, so the cap is what
    /// keeps a flat thousand-file directory from emitting a near-dense block.
    #[test]
    fn a_directory_over_the_cap_emits_only_the_capped_pairs() {
        let over = SIBLING.max_files_per_dir + 40;
        let fragments: Vec<Fragment> = (0..over)
            .map(|i| frag(&format!("src/f{i:04}.rs"), 1))
            .collect();

        let edges = SiblingEdgeBuilder.build(&fragments, None);
        let k = SIBLING.max_files_per_dir;
        // Every kept pair contributes a forward and a reverse edge.
        assert_eq!(edges.len(), k * (k - 1));
    }
}
