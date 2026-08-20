use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edges_from_ids};

fn is_erlang_file(path: &Path) -> bool {
    base::has_ext(path, &[".erl", ".hrl"])
}

static MODULE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^-module\((\w+)\)").unwrap());
static INCLUDE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^-include(?:_lib)?\(\s*"([^"]+)""#).unwrap());
static BEHAVIOUR_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^-behaviou?r\((\w+)\)").unwrap());
static IMPORT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^-import\((\w+),").unwrap());
static FUNC_DEF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^([a-z]\w*)\s*\(").unwrap());
static REMOTE_CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([a-z]\w*):(\w+)\s*\(").unwrap());

fn extract_refs(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();
    refs.extend(base::captures1(&INCLUDE_RE, content));
    refs.extend(
        BEHAVIOUR_RE
            .captures_iter(content)
            .map(|c| c[1].to_string()),
    );
    refs.extend(base::captures1(&IMPORT_RE, content));
    refs.extend(
        REMOTE_CALL_RE
            .captures_iter(content)
            .map(|c| c[1].to_string()),
    );
    refs
}

fn extract_modules(content: &str) -> FxHashSet<String> {
    base::captures1(&MODULE_RE, content).collect()
}

fn extract_func_defs(content: &str) -> FxHashSet<String> {
    base::captures1(&FUNC_DEF_RE, content).collect()
}

pub struct ErlangEdgeBuilder;

impl EdgeBuilder for ErlangEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_erlang_file(Path::new(f.path())))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let include_w = EDGE_WEIGHTS["erlang_include"].forward;
        let behaviour_w = EDGE_WEIGHTS["erlang_behaviour"].forward;
        let call_w = EDGE_WEIGHTS["erlang_call"].forward;
        let reverse_factor = EDGE_WEIGHTS["erlang_include"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let mut mod_to_frags: FxHashMap<String, Vec<_>> = FxHashMap::default();
        let mut fn_to_frags: FxHashMap<String, Vec<_>> = FxHashMap::default();
        for f in &frags {
            for m in extract_modules(&f.content) {
                mod_to_frags
                    .entry(m.to_lowercase())
                    .or_default()
                    .push(f.id.clone());
            }
            for name in extract_func_defs(&f.content) {
                fn_to_frags
                    .entry(name.to_lowercase())
                    .or_default()
                    .push(f.id.clone());
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            let self_fns = extract_func_defs(&f.content);
            for cap in INCLUDE_RE.captures_iter(&f.content) {
                base::link_by_name(&f.id, &cap[1], &idx, &mut edges, include_w, reverse_factor);
            }
            for cap in BEHAVIOUR_RE.captures_iter(&f.content) {
                if let Some(targets) = mod_to_frags.get(&cap[1].to_lowercase()) {
                    add_edges_from_ids(&mut edges, &f.id, targets, behaviour_w, reverse_factor);
                }
            }
            for cap in IMPORT_RE.captures_iter(&f.content) {
                if let Some(targets) = mod_to_frags.get(&cap[1].to_lowercase()) {
                    add_edges_from_ids(&mut edges, &f.id, targets, include_w, reverse_factor);
                }
            }
            for cap in REMOTE_CALL_RE.captures_iter(&f.content) {
                let module = &cap[1];
                if let Some(targets) = mod_to_frags.get(&module.to_lowercase()) {
                    add_edges_from_ids(&mut edges, &f.id, &targets, call_w, reverse_factor);
                }
            }
            for id in &f.identifiers {
                if self_fns.contains(id) {
                    continue;
                }
                if let Some(targets) = fn_to_frags.get(&id.to_lowercase()) {
                    add_edges_from_ids(&mut edges, &f.id, &targets, call_w, reverse_factor);
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
            is_erlang_file,
            extract_refs,
        )
    }
}
