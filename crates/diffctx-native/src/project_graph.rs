use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rustc_hash::FxHashSet;
use tracing::info;

use crate::candidate_files::collect_candidate_files;
use crate::config::limits::LIMITS;
use crate::edges;
use crate::fragmentation::process_files_for_fragments;
use crate::graph::{self, Graph};
use crate::types::{Fragment, FragmentId};

pub struct ProjectGraph {
    pub fragments: Vec<Fragment>,
    pub graph: Graph,
    pub root_dir: PathBuf,
}

pub fn build_project_graph(root_dir: &Path) -> Result<ProjectGraph> {
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
    let mut all_fragments = process_files_for_fragments(
        &candidate_files,
        &resolved_root,
        &[],
        &mut seen_frag_ids,
        None,
        false,
    );

    crate::pipeline::assign_token_counts(&mut all_fragments);

    info!(
        "project_graph: {} fragments from {} files",
        all_fragments.len(),
        candidate_files.len()
    );

    let skip_expensive = all_fragments.len() > LIMITS.skip_expensive_threshold;

    let capped = edges::collect_capped_edges(
        &all_fragments,
        Some(resolved_root.as_path()),
        skip_expensive,
        crate::deadline::Deadline::none(),
    );

    let graph = graph::build_graph_capped(&all_fragments, capped);

    Ok(ProjectGraph {
        fragments: all_fragments,
        graph,
        root_dir: resolved_root,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn git(dir: &Path, args: &[&str]) {
        let status = crate::git::git_command(dir)
            .args(args)
            .status()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(status.success(), "git {args:?} failed");
    }

    fn init_git_repo(dir: &Path) {
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
    }

    fn commit_all(dir: &Path) {
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "initial"]);
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
            pg.graph.node_count() >= 3,
            "expected fragments, got {}",
            pg.graph.node_count()
        );
        assert_eq!(pg.fragments.len(), pg.graph.node_count());
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
        assert_eq!(pg.graph.node_count(), 0);
        assert_eq!(pg.graph.edge_count(), 0);
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
        assert_eq!(pg.graph.categorized_edge_count(), pg.graph.edge_count());
    }
}
