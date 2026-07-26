use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, discover_files_by_refs};

fn is_r_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    matches!(ext.as_str(), ".r" | ".rmd")
}

static SOURCE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r##"(?m)source\s*\(\s*['"]([^'"]+)['"]"##).unwrap());
static FUNC_DEF_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*(\w+)\s*<-\s*function\s*\(").unwrap());
static S4_CLASS_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r##"(?m)setClass\s*\(\s*['"](\w+)['"]"##).unwrap());
static S4_METHOD_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r##"(?m)setMethod\s*\(\s*['"](\w+)['"]"##).unwrap());
static CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([a-zA-Z_.]\w*)\s*\(").unwrap());

static R_KEYWORDS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [
        "if",
        "else",
        "for",
        "while",
        "repeat",
        "function",
        "return",
        "next",
        "break",
        "in",
        "TRUE",
        "FALSE",
        "NULL",
        "NA",
        "Inf",
        "NaN",
        "library",
        "require",
        "source",
        "print",
        "cat",
        "paste",
        "c",
        "list",
        "data.frame",
        "matrix",
        "length",
        "nrow",
        "ncol",
        "which",
        "apply",
        "sapply",
        "lapply",
    ]
    .iter()
    .copied()
    .collect()
});

fn extract_sources(content: &str) -> FxHashSet<String> {
    SOURCE_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_defs(content: &str) -> FxHashSet<String> {
    let mut defs: FxHashSet<String> = FUNC_DEF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect();
    defs.extend(S4_CLASS_RE.captures_iter(content).map(|c| c[1].to_string()));
    defs.extend(
        S4_METHOD_RE
            .captures_iter(content)
            .map(|c| c[1].to_string()),
    );
    defs
}

fn extract_calls(content: &str) -> FxHashSet<String> {
    CALL_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .filter(|n| !R_KEYWORDS.contains(n.as_str()))
        .collect()
}

pub struct RLangEdgeBuilder;

impl EdgeBuilder for RLangEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_r_file(Path::new(f.path())))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let source_w = EDGE_WEIGHTS["r_source"].forward;
        let fn_w = EDGE_WEIGHTS["r_fn"].forward;
        let s4_w = EDGE_WEIGHTS["r_s4"].forward;
        let reverse_factor = EDGE_WEIGHTS["r_source"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let mut name_to_defs: FxHashMap<String, Vec<_>> = FxHashMap::default();
        for f in &frags {
            for name in extract_defs(&f.content) {
                name_to_defs
                    .entry(name.to_lowercase())
                    .or_default()
                    .push(f.id.clone());
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            let self_defs = extract_defs(&f.content);
            for src in extract_sources(&f.content) {
                base::link_by_name(&f.id, &src, &idx, &mut edges, source_w, reverse_factor);
            }
            for call in extract_calls(&f.content) {
                if self_defs.contains(&call) {
                    continue;
                }
                let w = if S4_CLASS_RE.is_match(&f.content) || S4_METHOD_RE.is_match(&f.content) {
                    s4_w
                } else {
                    fn_w
                };
                if let Some(targets) = name_to_defs.get(&call.to_lowercase()) {
                    for t in targets {
                        if t != &f.id {
                            add_edge(&mut edges, &f.id, t, w, reverse_factor);
                        }
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
        let r_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_r_file(f)).collect();
        if r_changed.is_empty() {
            return vec![];
        }
        let mut refs = FxHashSet::default();
        for f in &r_changed {
            if let Some(content) = base::read_file_cached(f, file_cache) {
                refs.extend(extract_sources(&content));
            }
        }
        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
