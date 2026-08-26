use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edges_from_ids};

fn is_r_file(path: &Path) -> bool {
    base::has_ext(path, &[".r", ".rmd"])
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
    base::kw(concat!(
        "if else for while repeat function return next break in TRUE FALSE NULL NA Inf NaN library ",
        "require source print cat paste c list data.frame matrix length nrow ncol which apply ",
        "sapply lapply ",
    ))
});
fn extract_sources(content: &str) -> FxHashSet<String> {
    base::captures1(&SOURCE_RE, content).collect()
}

fn extract_defs(content: &str) -> FxHashSet<String> {
    let mut defs: FxHashSet<String> = base::captures1(&FUNC_DEF_RE, content).collect();
    defs.extend(base::captures1(&S4_CLASS_RE, content));
    defs.extend(
        S4_METHOD_RE
            .captures_iter(content)
            .map(|c| c[1].to_string()),
    );
    defs
}

fn extract_calls(content: &str) -> FxHashSet<String> {
    base::captures1(&CALL_RE, content)
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
                    add_edges_from_ids(&mut edges, &f.id, &targets, w, reverse_factor);
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
        base::discover_by_extracted_refs(
            changed,
            candidates,
            repo_root,
            file_cache,
            is_r_file,
            extract_sources,
        )
    }
}
