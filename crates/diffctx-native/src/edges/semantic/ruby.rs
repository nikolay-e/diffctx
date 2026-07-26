use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::extensions::RUBY_EXTENSIONS;
use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, add_edges_from_ids, discover_files_by_refs};

fn is_ruby_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    RUBY_EXTENSIONS.contains(ext.as_str())
}

static REQUIRE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^\s*(?:require|require_relative)\s+['"]([^'"]+)['"]"#).unwrap());
static DEF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:class|module|def)\s+([A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)").unwrap()
});
static MIXIN_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:include|extend|prepend)\s+([A-Z]\w*(?:::[A-Z]\w*)*)").unwrap()
});
static CONST_REF_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b([A-Z][A-Za-z_]*(?:::[A-Z][A-Za-z_]*)*)\b").unwrap());

fn extract_requires(content: &str) -> FxHashSet<String> {
    REQUIRE_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_defines(content: &str) -> FxHashSet<String> {
    DEF_RE
        .captures_iter(content)
        .map(|c| {
            let name = &c[1];
            name.split("::").last().unwrap_or(name).to_string()
        })
        .collect()
}

fn extract_mixins(content: &str) -> FxHashSet<String> {
    MIXIN_RE
        .captures_iter(content)
        .map(|c| {
            let name = &c[1];
            name.split("::").last().unwrap_or(name).to_string()
        })
        .collect()
}

fn extract_const_refs(content: &str) -> FxHashSet<String> {
    CONST_REF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

pub struct RubyEdgeBuilder;

impl EdgeBuilder for RubyEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_ruby_file(Path::new(f.path())))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let require_w = EDGE_WEIGHTS["ruby_require"].forward;
        let include_w = EDGE_WEIGHTS["ruby_include"].forward;
        let const_w = EDGE_WEIGHTS["ruby_const"].forward;
        let reverse_factor = EDGE_WEIGHTS["ruby_require"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let mut name_to_defs: FxHashMap<String, Vec<_>> = FxHashMap::default();
        for f in &frags {
            for name in extract_defines(&f.content) {
                name_to_defs
                    .entry(name.to_lowercase())
                    .or_default()
                    .push(f.id.clone());
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            let self_defs = extract_defines(&f.content);
            for req in extract_requires(&f.content) {
                base::link_by_name(&f.id, &req, &idx, &mut edges, require_w, reverse_factor);
            }
            for mixin in extract_mixins(&f.content) {
                if let Some(targets) = name_to_defs.get(&mixin.to_lowercase()) {
                    add_edges_from_ids(&mut edges, &f.id, targets, include_w, reverse_factor);
                }
            }
            for cref in extract_const_refs(&f.content) {
                let leaf = cref.split("::").last().unwrap_or(&cref);
                if self_defs.contains(leaf) {
                    continue;
                }
                if let Some(targets) = name_to_defs.get(&leaf.to_lowercase()) {
                    for t in targets {
                        if t != &f.id {
                            add_edge(&mut edges, &f.id, t, const_w, reverse_factor);
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
        let rb_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_ruby_file(f)).collect();
        if rb_changed.is_empty() {
            return vec![];
        }
        let mut refs = FxHashSet::default();
        for f in &rb_changed {
            if let Some(content) = base::read_file_cached(f, file_cache) {
                refs.extend(extract_requires(&content));
            }
        }
        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
