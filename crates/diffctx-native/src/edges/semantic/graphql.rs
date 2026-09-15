use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edges_from_ids};

fn is_graphql_file(path: &Path) -> bool {
    base::has_ext(path, &[".graphql", ".gql"])
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
    base::captures1(&TYPE_DEF_RE, content).collect()
}

fn extract_type_refs(content: &str) -> FxHashSet<String> {
    let mut refs: FxHashSet<String> = base::captures1(&FIELD_TYPE_RE, content)
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
    base::captures1(&EXTEND_RE, content).collect()
}

pub struct GraphqlEdgeBuilder;

impl EdgeBuilder for GraphqlEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let Some(frags) = base::frags_where(fragments, |f| is_graphql_file(Path::new(f.path())))
        else {
            return FxHashMap::default();
        };

        let type_w = EDGE_WEIGHTS["graphql_type_ref"].forward;
        let extend_w = EDGE_WEIGHTS["graphql_extend"].forward;
        let reverse_factor = EDGE_WEIGHTS["graphql_type_ref"].reverse_factor;

        let mut name_to_defs: FxHashMap<String, Vec<_>> = FxHashMap::default();
        for f in &frags {
            base::index_lower(&mut name_to_defs, extract_defs(&f.content), &f.id);
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
                    add_edges_from_ids(&mut edges, &f.id, &targets, type_w, reverse_factor);
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
            |p| is_graphql_file(p),
            |c| extract_type_refs(c).into_iter().chain(extract_extends(c)),
        )
    }
}
