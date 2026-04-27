use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, discover_files_by_refs};

fn is_css_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    matches!(ext.as_str(), ".css" | ".scss" | ".less" | ".sass")
}

static IMPORT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^\s*@(?:import|use|forward)\s+['"]([^'"]+)['"]"#).unwrap());

fn extract_imports(content: &str) -> FxHashSet<String> {
    IMPORT_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

pub struct CssEdgeBuilder;

impl EdgeBuilder for CssEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_css_file(Path::new(f.path())))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let import_w = EDGE_WEIGHTS["css_import"].forward;
        let reverse_factor = EDGE_WEIGHTS["css_import"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            for imp in extract_imports(&f.content) {
                base::link_by_name(&f.id, &imp, &idx, &mut edges, import_w, reverse_factor);
            }
        }
        edges
    }

    fn discover_related_files(
        &self,
        changed: &[PathBuf],
        candidates: &[PathBuf],
        repo_root: Option<&Path>,
        file_cache: Option<&FxHashMap<PathBuf, String>>,
    ) -> Vec<PathBuf> {
        let css_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_css_file(f)).collect();
        if css_changed.is_empty() {
            return vec![];
        }
        let mut refs = FxHashSet::default();
        for f in &css_changed {
            if let Some(content) = base::read_file_cached(f, file_cache) {
                refs.extend(extract_imports(&content));
            }
        }
        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
