use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::extensions::PHP_EXTENSIONS;
use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, add_edges_from_ids, discover_files_by_refs};

fn is_php_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    PHP_EXTENSIONS.contains(ext.as_str())
}

static USE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*use\s+([\w\\]+)").unwrap());
static NAMESPACE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*namespace\s+([\w\\]+)").unwrap());
static REQUIRE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?m)^\s*(?:require_once|require|include_once|include)\s+['"]([^'"]+)['"]"#)
        .unwrap()
});
static DEF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:abstract\s+)?(?:class|interface|trait|enum)\s+([A-Z]\w*)").unwrap()
});
static FUNC_DEF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:public|protected|private|static)?\s*function\s+([a-zA-Z_]\w*)").unwrap()
});
static EXTENDS_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?:extends|implements)\s+([\w\\,\s]+)").unwrap());
static TYPE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w*)\b").unwrap());

fn extract_uses(content: &str) -> FxHashSet<String> {
    USE_RE
        .captures_iter(content)
        .map(|c| {
            let full = &c[1];
            full.split('\\').last().unwrap_or(full).to_string()
        })
        .collect()
}

fn extract_requires(content: &str) -> FxHashSet<String> {
    REQUIRE_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_namespace(content: &str) -> Option<String> {
    NAMESPACE_RE.captures(content).map(|c| c[1].to_string())
}

fn extract_defs(content: &str) -> FxHashSet<String> {
    let mut defs: FxHashSet<String> = DEF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect();
    defs.extend(FUNC_DEF_RE.captures_iter(content).map(|c| c[1].to_string()));
    defs
}

fn extract_inheritance(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();
    for cap in EXTENDS_RE.captures_iter(content) {
        for part in cap[1].split(',') {
            let name = part.trim().split('\\').last().unwrap_or("").trim();
            if !name.is_empty() {
                refs.insert(name.to_string());
            }
        }
    }
    refs
}

pub struct PhpEdgeBuilder;

impl EdgeBuilder for PhpEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_php_file(Path::new(f.path())))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let use_w = EDGE_WEIGHTS["php_use"].forward;
        let require_w = EDGE_WEIGHTS["php_require"].forward;
        let inherit_w = EDGE_WEIGHTS["php_inheritance"].forward;
        let type_w = EDGE_WEIGHTS["php_type"].forward;
        let reverse_factor = EDGE_WEIGHTS["php_use"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let mut name_to_defs: FxHashMap<String, Vec<_>> = FxHashMap::default();
        let mut ns_to_frags: FxHashMap<String, Vec<_>> = FxHashMap::default();
        for f in &frags {
            for name in extract_defs(&f.content) {
                name_to_defs
                    .entry(name.to_lowercase())
                    .or_default()
                    .push(f.id.clone());
            }
            if let Some(ns) = extract_namespace(&f.content) {
                ns_to_frags.entry(ns).or_default().push(f.id.clone());
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            let self_defs = extract_defs(&f.content);
            for req in extract_requires(&f.content) {
                base::link_by_name(&f.id, &req, &idx, &mut edges, require_w, reverse_factor);
            }
            for use_name in extract_uses(&f.content) {
                if let Some(targets) = name_to_defs.get(&use_name.to_lowercase()) {
                    add_edges_from_ids(&mut edges, &f.id, targets, use_w, reverse_factor);
                }
            }
            for parent in extract_inheritance(&f.content) {
                if let Some(targets) = name_to_defs.get(&parent.to_lowercase()) {
                    add_edges_from_ids(&mut edges, &f.id, targets, inherit_w, reverse_factor);
                }
            }
            for tr in TYPE_REF_RE.captures_iter(&f.content) {
                let name = &tr[1];
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
        let php_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_php_file(f)).collect();
        if php_changed.is_empty() {
            return vec![];
        }
        let mut refs = FxHashSet::default();
        for f in &php_changed {
            if let Some(content) = base::read_file_cached(f, file_cache) {
                refs.extend(extract_requires(&content));
                refs.extend(extract_uses(&content));
            }
        }
        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
