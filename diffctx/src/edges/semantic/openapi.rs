use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, discover_files_by_refs};

fn is_openapi_candidate(path: &Path) -> bool {
    let ext = base::file_ext(path);
    matches!(ext.as_str(), ".yaml" | ".yml" | ".json")
}

fn is_openapi_file(content: &str) -> bool {
    content
        .lines()
        .take(5)
        .any(|l| l.contains("openapi:") || l.contains("swagger:"))
}

static INTERNAL_REF_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"\$ref:\s*['"]?#/components/(\w+)/(\w+)"#).unwrap());
static EXTERNAL_REF_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"\$ref:\s*['"]?([^#'"]+)#"#).unwrap());
fn extract_internal_refs(content: &str) -> FxHashSet<String> {
    INTERNAL_REF_RE
        .captures_iter(content)
        .map(|c| c[2].to_string())
        .collect()
}

fn extract_external_refs(content: &str) -> FxHashSet<String> {
    EXTERNAL_REF_RE
        .captures_iter(content)
        .map(|c| c[1].trim().to_string())
        .collect()
}

fn extract_schema_defs(content: &str) -> FxHashSet<String> {
    let mut defs = FxHashSet::default();
    let mut in_components = false;
    let mut in_schemas = false;
    for line in content.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if indent == 0 && trimmed.starts_with("components:") {
            in_components = true;
            in_schemas = false;
            continue;
        }
        if indent == 0 && !trimmed.is_empty() {
            in_components = false;
            in_schemas = false;
            continue;
        }
        if in_components && indent == 2 && trimmed.starts_with("schemas:") {
            in_schemas = true;
            continue;
        }
        if in_components && indent == 2 && !trimmed.is_empty() {
            in_schemas = false;
            continue;
        }
        if in_schemas && indent == 4 {
            if let Some(name) = trimmed.strip_suffix(':') {
                let name = name.trim();
                if !name.is_empty() {
                    defs.insert(name.to_string());
                }
            }
        }
    }
    defs
}

pub struct OpenapiEdgeBuilder;

impl EdgeBuilder for OpenapiEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_openapi_candidate(Path::new(f.path())) && is_openapi_file(&f.content))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let internal_w = EDGE_WEIGHTS["openapi_internal_ref"].forward;
        let external_w = EDGE_WEIGHTS["openapi_external_ref"].forward;
        let reverse_factor = EDGE_WEIGHTS["openapi_internal_ref"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let mut schema_to_frags: FxHashMap<String, Vec<_>> = FxHashMap::default();
        for f in &frags {
            for name in extract_schema_defs(&f.content) {
                schema_to_frags
                    .entry(name.to_lowercase())
                    .or_default()
                    .push(f.id.clone());
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            for iref in extract_internal_refs(&f.content) {
                if let Some(targets) = schema_to_frags.get(&iref.to_lowercase()) {
                    for t in targets {
                        if t != &f.id {
                            add_edge(&mut edges, &f.id, t, internal_w, reverse_factor);
                        }
                    }
                }
            }
            for eref in extract_external_refs(&f.content) {
                base::link_by_name(&f.id, &eref, &idx, &mut edges, external_w, reverse_factor);
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
        let mut refs = FxHashSet::default();
        for f in changed {
            if !is_openapi_candidate(f) {
                continue;
            }
            if let Some(content) = base::read_file_cached(f, file_cache) {
                if !is_openapi_file(&content) {
                    continue;
                }
                refs.extend(extract_external_refs(&content));
            }
        }
        if refs.is_empty() {
            return vec![];
        }
        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
