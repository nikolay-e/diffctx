use std::path::Path;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, discover_by_refs};

fn is_julia_file(path: &Path) -> bool {
    base::has_ext(path, &[".jl"])
}

static USING_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*using\s+([\w.,\s]+)").unwrap());
static IMPORT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*import\s+([\w.,:\s]+)").unwrap());
static INCLUDE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^\s*include\s*\(\s*['"]([^'"]+)['"]"#).unwrap());
static STRUCT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*(?:mutable\s+)?struct\s+(\w+)").unwrap());
static ABSTRACT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*abstract\s+type\s+(\w+)").unwrap());
static FUNC_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*(?:function|macro)\s+(\w+)").unwrap());
static SHORT_FUNC_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^(\w+)\s*\(.*\)\s*=").unwrap());
static TYPE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w+)\b").unwrap());

fn extract_imports(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();
    for cap in USING_RE.captures_iter(content) {
        for part in cap[1].split(',') {
            let name = part.split(':').next().unwrap_or("").trim();
            if !name.is_empty() {
                refs.insert(name.to_string());
            }
        }
    }
    for cap in IMPORT_RE.captures_iter(content) {
        for part in cap[1].split(',') {
            let name = part.split(':').next().unwrap_or("").trim();
            if !name.is_empty() {
                refs.insert(name.to_string());
            }
        }
    }
    refs.extend(base::captures1(&INCLUDE_RE, content));
    refs
}

fn extract_defs(content: &str) -> FxHashSet<String> {
    let mut defs: FxHashSet<String> = base::captures1(&STRUCT_RE, content).collect();
    defs.extend(base::captures1(&ABSTRACT_RE, content));
    defs.extend(base::captures1(&FUNC_RE, content));
    defs.extend(
        SHORT_FUNC_RE
            .captures_iter(content)
            .map(|c| c[1].to_string()),
    );
    defs
}

pub(crate) struct JuliaEdgeBuilder;

impl EdgeBuilder for JuliaEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let Some(frags) = base::frags_where(fragments, |f| is_julia_file(Path::new(f.path())))
        else {
            return FxHashMap::default();
        };

        let using_w = EDGE_WEIGHTS["julia_using"].forward;
        let include_w = EDGE_WEIGHTS["julia_include"].forward;
        let type_w = EDGE_WEIGHTS["julia_type"].forward;
        let fn_w = EDGE_WEIGHTS["julia_fn"].forward;
        let reverse_factor = EDGE_WEIGHTS["julia_using"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let defs = base::DefIndex::new(&frags, reverse_factor, extract_defs);

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            let self_defs = extract_defs(&f.content);
            for imp in extract_imports(&f.content) {
                let w = if INCLUDE_RE.is_match(&format!("include(\"{}\")", imp)) {
                    include_w
                } else {
                    using_w
                };
                base::link_by_name(&f.id, &imp, &idx, &mut edges, w, reverse_factor);
            }
            let type_refs = base::captures1(&TYPE_REF_RE, &f.content);
            defs.link_except(&mut edges, &f.id, type_refs, &self_defs, type_w);
            defs.link_except(&mut edges, &f.id, &f.identifiers, &self_defs, fn_w);
        }
        edges
    }

    discover_by_refs!(is_julia_file, extract_imports);
}
