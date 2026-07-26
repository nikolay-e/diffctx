use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, discover_files_by_refs};

fn is_dbt_file(content: &str) -> bool {
    content.contains("{{ ref(") || content.contains("{{ source(") || content.contains("{{ config(")
}

fn is_sql_file(path: &Path) -> bool {
    base::file_ext(path) == ".sql"
}

static REF_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"\{\{\s*ref\s*\(\s*['"](\w+)['"]\s*\)\s*\}\}"#).unwrap());
static SOURCE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"\{\{\s*source\s*\(\s*['"](\w+)['"]\s*,\s*['"](\w+)['"]\s*\)\s*\}\}"#).unwrap()
});
static MACRO_CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\{\{\s*(\w+)\s*\(").unwrap());
static MACRO_DEF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\{%-?\s*macro\s+(\w+)").unwrap());

static DBT_BUILTINS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [
        "ref", "source", "config", "set", "if", "for", "endif", "endfor", "else", "elif", "block",
        "endblock", "macro", "endmacro", "do", "call", "filter",
    ]
    .iter()
    .copied()
    .collect()
});

fn extract_refs(content: &str) -> FxHashSet<String> {
    REF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_sources(content: &str) -> FxHashSet<String> {
    SOURCE_RE
        .captures_iter(content)
        .map(|c| c[2].to_string())
        .collect()
}

fn extract_macro_calls(content: &str) -> FxHashSet<String> {
    MACRO_CALL_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .filter(|n| !DBT_BUILTINS.contains(n.as_str()))
        .collect()
}

fn extract_macro_defs(content: &str) -> FxHashSet<String> {
    MACRO_DEF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

pub struct DbtEdgeBuilder;

impl EdgeBuilder for DbtEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_sql_file(Path::new(f.path())) && is_dbt_file(&f.content))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let ref_w = EDGE_WEIGHTS["dbt_ref"].forward;
        let source_w = EDGE_WEIGHTS["dbt_source"].forward;
        let macro_w = EDGE_WEIGHTS["dbt_macro"].forward;
        let ref_rev = EDGE_WEIGHTS["dbt_ref"].reverse_factor;
        let source_rev = EDGE_WEIGHTS["dbt_source"].reverse_factor;
        let macro_rev = EDGE_WEIGHTS["dbt_macro"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let mut macro_to_frags: FxHashMap<String, Vec<_>> = FxHashMap::default();
        for f in &frags {
            for name in extract_macro_defs(&f.content) {
                macro_to_frags
                    .entry(name.to_lowercase())
                    .or_default()
                    .push(f.id.clone());
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            for r in extract_refs(&f.content) {
                base::link_by_name(&f.id, &r, &idx, &mut edges, ref_w, ref_rev);
            }
            for s in extract_sources(&f.content) {
                base::link_by_name(&f.id, &s, &idx, &mut edges, source_w, source_rev);
            }
            for mc in extract_macro_calls(&f.content) {
                if let Some(targets) = macro_to_frags.get(&mc.to_lowercase()) {
                    for t in targets {
                        if t != &f.id {
                            add_edge(&mut edges, &f.id, t, macro_w, macro_rev);
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
        let mut refs = FxHashSet::default();
        for f in changed {
            if !is_sql_file(f) {
                continue;
            }
            if let Some(content) = base::read_file_cached(f, file_cache) {
                if !is_dbt_file(&content) {
                    continue;
                }
                refs.extend(extract_refs(&content));
                refs.extend(extract_sources(&content));
                refs.extend(extract_macro_calls(&content));
            }
        }
        if refs.is_empty() {
            return vec![];
        }
        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
