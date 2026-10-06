use std::path::Path;

use rustc_hash::FxHashMap;

use crate::config::limits::COCHANGE;
use crate::config::weights::EDGE_WEIGHTS;
use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{EdgeBuilder, add_edge};

pub struct CochangeEdgeBuilder;

impl CochangeEdgeBuilder {
    /// The commits up to the range's base — not whatever is checked out, and
    /// not the range's own commits — under the git timeout; `-z` keeps
    /// non-ASCII paths unquoted (#340).
    fn get_git_log_files(&self, repo_root: &Path) -> Option<Vec<Vec<String>>> {
        let head = crate::resource::current_head_rev().unwrap_or_else(|| "HEAD".to_string());
        let limit = format!("-n{}", COCHANGE.commits_limit);
        let stdout = crate::git::run_git(
            repo_root,
            &[
                "log",
                "--name-only",
                "-z",
                "--format=%x1e",
                &limit,
                &head,
                "--",
            ],
        )
        .ok()?;
        let commits: Vec<Vec<String>> = stdout
            .split('\u{1e}')
            .map(|c| {
                c.split(['\0', '\n'])
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<String>>()
            })
            .filter(|files| !files.is_empty())
            .collect();

        Some(commits)
    }

    fn count_cochanges(&self, commits: &[Vec<String>]) -> FxHashMap<(String, String), usize> {
        let mut cochange: FxHashMap<(String, String), usize> = FxHashMap::default();
        for files in commits {
            if files.len() > COCHANGE.max_files_per_commit {
                continue;
            }
            for i in 0..files.len() {
                for j in (i + 1)..files.len() {
                    let pair = if files[i] < files[j] {
                        (files[i].clone(), files[j].clone())
                    } else {
                        (files[j].clone(), files[i].clone())
                    };
                    *cochange.entry(pair).or_insert(0) += 1;
                }
            }
        }
        cochange
    }

    fn representatives_by_rel_path(
        &self,
        fragments: &[Fragment],
        repo_root: &Path,
    ) -> FxHashMap<String, FragmentId> {
        crate::edges::base::file_representatives(fragments)
            .into_iter()
            .filter_map(|(path, id)| {
                let p = Path::new(&path);
                let rel = if p.is_absolute() {
                    p.strip_prefix(repo_root)
                        .ok()
                        .map(|r| crate::paths::to_posix_display(r.to_string_lossy()))
                } else {
                    Some(crate::paths::to_posix_display(p.to_string_lossy()))
                };
                rel.map(|r| (r, id))
            })
            .collect()
    }
}

impl EdgeBuilder for CochangeEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let repo_root = match repo_root {
            Some(r) => r,
            None => return FxHashMap::default(),
        };

        let weight = EDGE_WEIGHTS["cochange"].forward;
        let reverse_factor = EDGE_WEIGHTS["cochange"].reverse_factor;

        let commits = match self.get_git_log_files(repo_root) {
            Some(c) => c,
            None => return FxHashMap::default(),
        };

        let cochange = self.count_cochanges(&commits);
        let representatives = self.representatives_by_rel_path(fragments, repo_root);

        // Co-change endorses a file, not each of its fragments: the pair
        // links the two files' representatives, the way every file-level
        // relation does (`base::file_representatives`).
        let mut edges: EdgeDict = FxHashMap::default();
        for ((p1, p2), count) in &cochange {
            if *count < COCHANGE.min_count {
                continue;
            }
            let edge_weight = weight.min(COCHANGE.log_scale_factor * (*count as f64).ln_1p());
            if let (Some(fid1), Some(fid2)) = (representatives.get(p1), representatives.get(p2)) {
                if fid1 != fid2 {
                    add_edge(&mut edges, fid1, fid2, edge_weight, reverse_factor);
                }
            }
        }

        edges
    }
}
