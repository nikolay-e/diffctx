use std::path::Path;
use std::process::Command;

use rustc_hash::FxHashMap;

use crate::config::limits::COCHANGE;
use crate::config::weights::EDGE_WEIGHTS;
use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{EdgeBuilder, add_edge};

pub struct CochangeEdgeBuilder;

impl CochangeEdgeBuilder {
    fn get_git_log_files(&self, repo_root: &Path) -> Option<Vec<Vec<String>>> {
        let output = Command::new("git")
            .args([
                "-C",
                &repo_root.to_string_lossy(),
                "log",
                "--name-only",
                "--pretty=format:",
                &format!("-n{}", COCHANGE.commits_limit),
            ])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let commits: Vec<Vec<String>> = stdout
            .split("\n\n")
            .filter(|c| !c.trim().is_empty())
            .map(|c| {
                c.trim()
                    .split('\n')
                    .filter(|l| !l.is_empty())
                    .map(|l| l.to_string())
                    .collect()
            })
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

    fn build_path_to_frags_index(
        &self,
        fragments: &[Fragment],
        repo_root: &Path,
    ) -> FxHashMap<String, Vec<FragmentId>> {
        let mut path_to_frags: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        for f in fragments {
            let path = Path::new(f.path());
            let rel = if path.is_absolute() {
                path.strip_prefix(repo_root)
                    .ok()
                    .map(|r| r.to_string_lossy().replace('\\', "/"))
            } else {
                Some(path.to_string_lossy().replace('\\', "/"))
            };
            if let Some(rel) = rel {
                path_to_frags.entry(rel).or_default().push(f.id.clone());
            }
        }
        path_to_frags
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
        let path_to_frags = self.build_path_to_frags_index(fragments, repo_root);

        let mut edges: EdgeDict = FxHashMap::default();
        for ((p1, p2), count) in &cochange {
            if *count < COCHANGE.min_count {
                continue;
            }
            let edge_weight = weight.min(0.1 * (*count as f64).ln_1p());
            for fid1 in path_to_frags.get(p1).unwrap_or(&vec![]) {
                for fid2 in path_to_frags.get(p2).unwrap_or(&vec![]) {
                    if fid1 == fid2 {
                        continue;
                    }
                    add_edge(&mut edges, fid1, fid2, edge_weight, reverse_factor);
                }
            }
        }

        edges
    }

    fn is_expensive(&self) -> bool {
        true
    }
}
