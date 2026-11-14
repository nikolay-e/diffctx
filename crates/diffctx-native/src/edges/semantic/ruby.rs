use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::extensions::RUBY_EXTENSIONS;
use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder};

fn is_ruby_file(path: &Path) -> bool {
    RUBY_EXTENSIONS.contains(crate::languages::file_ext(path).as_str())
}

const RUBY_SUFFIXES: &[&str] = &[".rb", ""];

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
    base::captures1(&REQUIRE_RE, content).collect()
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
    base::captures1(&CONST_REF_RE, content).collect()
}

pub(crate) struct RubyEdgeBuilder;

impl EdgeBuilder for RubyEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let Some(frags) = base::frags_where(fragments, |f| is_ruby_file(Path::new(f.path())))
        else {
            return FxHashMap::default();
        };

        let require_w = EDGE_WEIGHTS["ruby_require"].forward;
        let include_w = EDGE_WEIGHTS["ruby_include"].forward;
        let const_w = EDGE_WEIGHTS["ruby_const"].forward;
        let reverse_factor = EDGE_WEIGHTS["ruby_require"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let defs = base::DefIndex::new(&frags, reverse_factor, extract_defines);

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            let self_defs = extract_defines(&f.content);
            for req in extract_requires(&f.content) {
                // A path (`require_relative '../vendor/palette'`) names a
                // file; anything else is a load-path name.
                if !base::link_module_path(
                    &f.id,
                    &req,
                    RUBY_SUFFIXES,
                    &idx,
                    &mut edges,
                    require_w,
                    reverse_factor,
                ) {
                    base::link_by_name(&f.id, &req, &idx, &mut edges, require_w, reverse_factor);
                }
            }
            defs.link(&mut edges, &f.id, extract_mixins(&f.content), include_w);
            let leaves = extract_const_refs(&f.content)
                .into_iter()
                .map(|cref| cref.rsplit("::").next().unwrap_or(&cref).to_string());
            defs.link_except(&mut edges, &f.id, leaves, &self_defs, const_w);
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
        let mut found = base::discover_by_module_paths(
            changed,
            candidates,
            repo_root,
            file_cache,
            is_ruby_file,
            extract_requires,
            RUBY_SUFFIXES,
        );
        for path in base::discover_by_extracted_refs(
            changed,
            candidates,
            repo_root,
            file_cache,
            is_ruby_file,
            extract_requires,
        ) {
            if !found.contains(&path) {
                found.push(path);
            }
        }
        found
    }
}
