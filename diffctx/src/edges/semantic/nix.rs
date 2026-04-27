use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, discover_files_by_refs};

fn is_nix_file(path: &Path) -> bool {
    base::file_ext(path) == ".nix"
}

static IMPORT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"import\s+([\./][\w./-]+)").unwrap());
static CALL_PACKAGE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"callPackage\s+([\./][\w./-]+)").unwrap());
static FILE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"\./[\w./-]+"#).unwrap());

fn extract_refs(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();
    refs.extend(IMPORT_RE.captures_iter(content).map(|c| c[1].to_string()));
    refs.extend(
        CALL_PACKAGE_RE
            .captures_iter(content)
            .map(|c| c[1].to_string()),
    );
    for m in FILE_REF_RE.find_iter(content) {
        refs.insert(m.as_str().to_string());
    }
    refs
}

pub struct NixEdgeBuilder;

impl EdgeBuilder for NixEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_nix_file(Path::new(f.path())))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let import_w = EDGE_WEIGHTS["nix_import"].forward;
        let reverse_factor = EDGE_WEIGHTS["nix_import"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            for r in extract_refs(&f.content) {
                base::link_by_name(&f.id, &r, &idx, &mut edges, import_w, reverse_factor);
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
        let nix_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_nix_file(f)).collect();
        if nix_changed.is_empty() {
            return vec![];
        }
        let mut refs = FxHashSet::default();
        for f in &nix_changed {
            if let Some(content) = base::read_file_cached(f, file_cache) {
                refs.extend(extract_refs(&content));
            }
        }
        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
