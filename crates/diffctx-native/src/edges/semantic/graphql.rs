use std::path::Path;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, discover_by_refs};

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

        let defs = base::DefIndex::new(&frags, reverse_factor, extract_defs);

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            let self_defs = extract_defs(&f.content);
            defs.link(&mut edges, &f.id, extract_extends(&f.content), extend_w);
            let trefs = extract_type_refs(&f.content);
            defs.link_except(&mut edges, &f.id, trefs, &self_defs, type_w);
        }
        edges
    }

    discover_by_refs!(|p| is_graphql_file(p), |c| extract_type_refs(c)
        .into_iter()
        .chain(extract_extends(c)));
}
