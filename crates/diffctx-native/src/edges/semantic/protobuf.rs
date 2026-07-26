use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, discover_files_by_refs};

fn is_proto_file(path: &Path) -> bool {
    base::file_ext(path) == ".proto"
}

static IMPORT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^\s*import\s+(?:public\s+)?["']([^"']+)["']"#).unwrap());
static MSG_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*(?:message|enum|service)\s+(\w+)").unwrap());
static FIELD_TYPE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:repeated\s+|optional\s+|required\s+|map<\w+,\s*)?([A-Z]\w+)").unwrap()
});
static RPC_TYPE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?:returns\s*\(|rpc\s+\w+\s*\()\s*(?:stream\s+)?([A-Z]\w+)").unwrap()
});

fn extract_imports(content: &str) -> FxHashSet<String> {
    IMPORT_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_defs(content: &str) -> FxHashSet<String> {
    MSG_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_type_refs(content: &str) -> FxHashSet<String> {
    let mut refs: FxHashSet<String> = FIELD_TYPE_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect();
    refs.extend(RPC_TYPE_RE.captures_iter(content).map(|c| c[1].to_string()));
    refs
}

pub struct ProtobufEdgeBuilder;

impl EdgeBuilder for ProtobufEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_proto_file(Path::new(f.path())))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let import_w = EDGE_WEIGHTS["proto_import"].forward;
        let msg_w = EDGE_WEIGHTS["proto_message_ref"].forward;
        let rpc_w = EDGE_WEIGHTS["proto_service_rpc"].forward;
        let reverse_factor = EDGE_WEIGHTS["proto_import"].reverse_factor;

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
                base::link_by_name(&f.id, &imp, &idx, &mut edges, import_w, reverse_factor);
            }
            for tref in extract_type_refs(&f.content) {
                if self_defs.contains(&tref) {
                    continue;
                }
                let w = if RPC_TYPE_RE.captures_iter(&f.content).any(|c| c[1] == tref) {
                    rpc_w
                } else {
                    msg_w
                };
                if let Some(targets) = name_to_defs.get(&tref.to_lowercase()) {
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
        let proto_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_proto_file(f)).collect();
        if proto_changed.is_empty() {
            return vec![];
        }
        let mut refs = FxHashSet::default();
        for f in &proto_changed {
            if let Some(content) = base::read_file_cached(f, file_cache) {
                refs.extend(extract_imports(&content));
            }
        }
        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
