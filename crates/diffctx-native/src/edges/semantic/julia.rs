use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, discover_files_by_refs};

fn is_julia_file(path: &Path) -> bool {
    base::file_ext(path) == ".jl"
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
    refs.extend(INCLUDE_RE.captures_iter(content).map(|c| c[1].to_string()));
    refs
}

fn extract_defs(content: &str) -> FxHashSet<String> {
    let mut defs: FxHashSet<String> = STRUCT_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect();
    defs.extend(ABSTRACT_RE.captures_iter(content).map(|c| c[1].to_string()));
    defs.extend(FUNC_RE.captures_iter(content).map(|c| c[1].to_string()));
    defs.extend(
        SHORT_FUNC_RE
            .captures_iter(content)
            .map(|c| c[1].to_string()),
    );
    defs
}

pub struct JuliaEdgeBuilder;

impl EdgeBuilder for JuliaEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_julia_file(Path::new(f.path())))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let using_w = EDGE_WEIGHTS["julia_using"].forward;
        let include_w = EDGE_WEIGHTS["julia_include"].forward;
        let type_w = EDGE_WEIGHTS["julia_type"].forward;
        let fn_w = EDGE_WEIGHTS["julia_fn"].forward;
        let reverse_factor = EDGE_WEIGHTS["julia_using"].reverse_factor;

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
            for imp in extract_imports(&f.content) {
                let w = if INCLUDE_RE.is_match(&format!("include(\"{}\")", imp)) {
                    include_w
                } else {
                    using_w
                };
                base::link_by_name(&f.id, &imp, &idx, &mut edges, w, reverse_factor);
            }
            for cap in TYPE_REF_RE.captures_iter(&f.content) {
                let name = &cap[1];
                if self_defs.contains(name) {
                    continue;
                }
                if let Some(targets) = name_to_defs.get(&name.to_lowercase()) {
                    for t in targets {
                        if t != &f.id {
                            add_edge(&mut edges, &f.id, t, type_w, reverse_factor);
                        }
                    }
                }
            }
            for id in &f.identifiers {
                if self_defs.contains(id) {
                    continue;
                }
                if let Some(targets) = name_to_defs.get(&id.to_lowercase()) {
                    for t in targets {
                        if t != &f.id {
                            add_edge(&mut edges, &f.id, t, fn_w, reverse_factor);
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
        let jl_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_julia_file(f)).collect();
        if jl_changed.is_empty() {
            return vec![];
        }
        let mut refs = FxHashSet::default();
        for f in &jl_changed {
            if let Some(content) = base::read_file_cached(f, file_cache) {
                refs.extend(extract_imports(&content));
            }
        }
        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
