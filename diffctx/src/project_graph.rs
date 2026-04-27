use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rayon::prelude::*;
use rustc_hash::FxHashSet;
use tracing::info;

use crate::candidate_files::collect_candidate_files;
use crate::config::limits::LIMITS;
use crate::edges::{self, EdgeCategories};
use crate::fragmentation::process_files_for_fragments;
use crate::git::CatFileBatch;
use crate::graph::{self, Graph};
use crate::tokenizer::count_tokens;
use crate::types::{Fragment, FragmentId};

pub struct ProjectGraph {
    pub fragments: Vec<Fragment>,
    pub graph: Graph,
    pub edge_categories: EdgeCategories,
    pub root_dir: PathBuf,
}

impl ProjectGraph {
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }
}

pub struct ProjectGraphOptions {
    pub use_git_batch_reader: bool,
    pub skip_expensive_edges: Option<bool>,
}

impl Default for ProjectGraphOptions {
    fn default() -> Self {
        Self {
            use_git_batch_reader: false,
            skip_expensive_edges: None,
        }
    }
}

pub fn build_project_graph(root_dir: &Path) -> Result<ProjectGraph> {
    build_project_graph_with_options(root_dir, &ProjectGraphOptions::default())
}

pub fn build_project_graph_with_options(
    root_dir: &Path,
    options: &ProjectGraphOptions,
) -> Result<ProjectGraph> {
    let resolved_root = root_dir
        .canonicalize()
        .with_context(|| format!("failed to canonicalize root_dir '{}'", root_dir.display()))?;

    let included_set: FxHashSet<PathBuf> = FxHashSet::default();
    let candidate_files = collect_candidate_files(&resolved_root, &included_set);

    info!(
        "project_graph: found {} candidate files",
        candidate_files.len()
    );

    let mut seen_frag_ids: FxHashSet<FragmentId> = FxHashSet::default();
    let mut all_fragments = if options.use_git_batch_reader {
        let mut batch_reader = CatFileBatch::new(&resolved_root)?;
        let frags = process_files_for_fragments(
            &candidate_files,
            &resolved_root,
            &[],
            &mut seen_frag_ids,
            Some(&mut batch_reader),
        );
        batch_reader.close();
        frags
    } else {
        process_files_for_fragments(
            &candidate_files,
            &resolved_root,
            &[],
            &mut seen_frag_ids,
            None,
        )
    };

    assign_token_counts(&mut all_fragments);

    info!(
        "project_graph: {} fragments from {} files",
        all_fragments.len(),
        candidate_files.len()
    );

    let skip_expensive = options
        .skip_expensive_edges
        .unwrap_or_else(|| all_fragments.len() > LIMITS.skip_expensive_threshold);

    let (edges, categories) = edges::collect_all_edges(
        &all_fragments,
        Some(resolved_root.as_path()),
        skip_expensive,
    );

    let edge_categories_snapshot = categories.clone();
    let graph = graph::build_graph(&all_fragments, edges, categories);

    Ok(ProjectGraph {
        fragments: all_fragments,
        graph,
        edge_categories: edge_categories_snapshot,
        root_dir: resolved_root,
    })
}

fn assign_token_counts(fragments: &mut [Fragment]) {
    fragments.par_iter_mut().for_each(|frag| {
        if frag.token_count == 0 {
            frag.token_count = count_tokens(&frag.content) + LIMITS.overhead_per_fragment;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_git_repo(dir: &Path) {
        Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir)
            .status()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(dir)
            .status()
            .expect("git config email");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir)
            .status()
            .expect("git config name");
    }

    fn commit_all(dir: &Path) {
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .status()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-q", "-m", "initial"])
            .current_dir(dir)
            .status()
            .expect("git commit");
    }

    fn write_file(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&path, content).expect("write file");
    }

    #[test]
    fn build_project_graph_on_tiny_python_project() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        init_git_repo(root);
        write_file(
            root,
            "alpha.py",
            "def alpha():\n    return beta()\n\ndef beta():\n    return 1\n",
        );
        write_file(
            root,
            "consumer.py",
            "from alpha import alpha\n\ndef main():\n    return alpha()\n",
        );
        commit_all(root);

        let pg = build_project_graph(root).expect("build_project_graph");
        assert!(
            pg.node_count() >= 3,
            "expected fragments, got {}",
            pg.node_count()
        );
        assert_eq!(pg.fragments.len(), pg.node_count());
        assert!(pg.root_dir.is_absolute());
    }

    #[test]
    fn build_project_graph_empty_dir_yields_no_fragments() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        init_git_repo(root);
        write_file(root, ".gitkeep", "");
        commit_all(root);

        let pg = build_project_graph(root).expect("build_project_graph");
        assert_eq!(pg.fragments.len(), 0);
        assert_eq!(pg.node_count(), 0);
        assert_eq!(pg.edge_count(), 0);
    }

    #[test]
    fn build_project_graph_produces_edge_categories() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        init_git_repo(root);
        write_file(
            root,
            "lib.py",
            "def helper(x):\n    return x + 1\n\ndef other(y):\n    return helper(y) * 2\n",
        );
        write_file(
            root,
            "app.py",
            "from lib import helper, other\n\ndef run():\n    return helper(other(3))\n",
        );
        commit_all(root);

        let pg = build_project_graph(root).expect("build_project_graph");
        assert!(pg.fragments.len() >= 2);
        assert_eq!(pg.edge_categories.len(), pg.graph.edge_count());
    }
}
