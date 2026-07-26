use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, discover_files_by_refs};

fn is_latex_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    matches!(ext.as_str(), ".tex" | ".sty" | ".cls" | ".bib")
}

static INPUT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\\(?:input|include)\{([^}]+)\}").unwrap());
static USEPACKAGE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\\usepackage(?:\[.*?\])?\{([^}]+)\}").unwrap());
static BIB_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\\(?:bibliography|addbibresource)\{([^}]+)\}").unwrap());
static SUBFILE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\\(?:subfile|subimport\{[^}]*\})\{([^}]+)\}").unwrap());

fn extract_refs(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();
    for re in [&*INPUT_RE, &*SUBFILE_RE] {
        refs.extend(re.captures_iter(content).map(|c| c[1].to_string()));
    }
    for cap in USEPACKAGE_RE.captures_iter(content) {
        for pkg in cap[1].split(',') {
            let name = pkg.trim();
            if !name.is_empty() {
                refs.insert(name.to_string());
            }
        }
    }
    for cap in BIB_RE.captures_iter(content) {
        for bib in cap[1].split(',') {
            let name = bib.trim();
            if !name.is_empty() {
                refs.insert(name.to_string());
            }
        }
    }
    refs
}

pub struct LatexEdgeBuilder;

impl EdgeBuilder for LatexEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_latex_file(Path::new(f.path())))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let input_w = EDGE_WEIGHTS["latex_input"].forward;
        let pkg_w = EDGE_WEIGHTS["latex_package"].forward;
        let bib_w = EDGE_WEIGHTS["latex_bib"].forward;
        let reverse_factor = EDGE_WEIGHTS["latex_input"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            for cap in INPUT_RE.captures_iter(&f.content) {
                base::link_by_name(&f.id, &cap[1], &idx, &mut edges, input_w, reverse_factor);
            }
            for cap in SUBFILE_RE.captures_iter(&f.content) {
                base::link_by_name(&f.id, &cap[1], &idx, &mut edges, input_w, reverse_factor);
            }
            for cap in USEPACKAGE_RE.captures_iter(&f.content) {
                for pkg in cap[1].split(',') {
                    let name = pkg.trim();
                    if !name.is_empty() {
                        base::link_by_name(&f.id, name, &idx, &mut edges, pkg_w, reverse_factor);
                    }
                }
            }
            for cap in BIB_RE.captures_iter(&f.content) {
                for bib in cap[1].split(',') {
                    let name = bib.trim();
                    if !name.is_empty() {
                        base::link_by_name(&f.id, name, &idx, &mut edges, bib_w, reverse_factor);
                    }
                }
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
        let tex_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_latex_file(f)).collect();
        if tex_changed.is_empty() {
            return vec![];
        }
        let mut refs = FxHashSet::default();
        for f in &tex_changed {
            if let Some(content) = base::read_file_cached(f, file_cache) {
                refs.extend(extract_refs(&content));
            }
        }
        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
