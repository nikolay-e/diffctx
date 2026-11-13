use std::path::Path;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, discover_by_refs};

fn is_css_file(path: &Path) -> bool {
    base::has_ext(path, &[".css", ".scss", ".less", ".sass"])
}

static IMPORT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^\s*@(?:import|use|forward)\s+['"]([^'"]+)['"]"#).unwrap());

fn extract_imports(content: &str) -> FxHashSet<String> {
    base::captures1(&IMPORT_RE, content).collect()
}

pub struct CssEdgeBuilder;

impl EdgeBuilder for CssEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let Some(frags) = base::frags_where(fragments, |f| is_css_file(Path::new(f.path()))) else {
            return FxHashMap::default();
        };

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

    discover_by_refs!(is_css_file, extract_imports);
}
