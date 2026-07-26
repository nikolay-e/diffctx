use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, add_edges_from_ids, discover_files_by_refs};

fn is_graphql_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    ext == ".graphql" || ext == ".gql"
}

static TYPE_DEF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:type|input|interface|enum|union|scalar)\s+(\w+)").unwrap()
});
static EXTEND_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*extend\s+(?:type|input|interface|enum|union)\s+(\w+)").unwrap()
});
static FIELD_TYPE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r":\s*\[?([A-Z]\w+)").unwrap());
static IMPLEMENTS_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"implements\s+([\w\s&]+)").unwrap());
static UNION_MEMBERS_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"union\s+\w+\s*=\s*([\w\s|]+)").unwrap());

static GQL_BUILTINS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    ["String", "Int", "Float", "Boolean", "ID"]
        .iter()
        .copied()
        .collect()
});

fn extract_defs(content: &str) -> FxHashSet<String> {
    TYPE_DEF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_type_refs(content: &str) -> FxHashSet<String> {
    let mut refs: FxHashSet<String> = FIELD_TYPE_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .filter(|n| !GQL_BUILTINS.contains(n.as_str()))
        .collect();
    for cap in IMPLEMENTS_RE.captures_iter(content) {
        for part in cap[1].split('&') {
            let name = part.trim();
            if !name.is_empty() {
                refs.insert(name.to_string());
            }
        }
    }
    for cap in UNION_MEMBERS_RE.captures_iter(content) {
        for part in cap[1].split('|') {
            let name = part.trim();
            if !name.is_empty() {
                refs.insert(name.to_string());
            }
        }
    }
    refs
}

fn extract_extends(content: &str) -> FxHashSet<String> {
    EXTEND_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

pub struct GraphqlEdgeBuilder;

impl EdgeBuilder for GraphqlEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_graphql_file(Path::new(f.path())))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let type_w = EDGE_WEIGHTS["graphql_type_ref"].forward;
        let extend_w = EDGE_WEIGHTS["graphql_extend"].forward;
        let reverse_factor = EDGE_WEIGHTS["graphql_type_ref"].reverse_factor;

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
            for ext_name in extract_extends(&f.content) {
                if let Some(targets) = name_to_defs.get(&ext_name.to_lowercase()) {
                    add_edges_from_ids(&mut edges, &f.id, targets, extend_w, reverse_factor);
                }
            }
            for tref in extract_type_refs(&f.content) {
                if self_defs.contains(&tref) {
                    continue;
                }
                if let Some(targets) = name_to_defs.get(&tref.to_lowercase()) {
                    for t in targets {
                        if t != &f.id {
                            add_edge(&mut edges, &f.id, t, type_w, reverse_factor);
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
        let gql_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_graphql_file(f)).collect();
        if gql_changed.is_empty() {
            return vec![];
        }
        let mut refs = FxHashSet::default();
        for f in &gql_changed {
            if let Some(content) = base::read_file_cached(f, file_cache) {
                refs.extend(extract_type_refs(&content));
                refs.extend(extract_extends(&content));
            }
        }
        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
