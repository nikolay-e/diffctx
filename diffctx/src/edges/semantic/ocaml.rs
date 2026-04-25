use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, add_edges_from_ids, discover_files_by_refs};

fn is_ocaml_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    ext == ".ml" || ext == ".mli"
}

static OPEN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*open\s+([A-Z]\w*)").unwrap());
static INCLUDE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*include\s+([A-Z]\w*)").unwrap());
static MODULE_DEF_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*module\s+([A-Z]\w*)").unwrap());
static LET_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*let\s+(\w+)").unwrap());
static VAL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*val\s+(\w+)").unwrap());
static TYPE_DEF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*type\s+(\w+)").unwrap());
static MODULE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w*)\.\w+").unwrap());

fn extract_opens(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();
    refs.extend(OPEN_RE.captures_iter(content).map(|c| c[1].to_string()));
    refs.extend(INCLUDE_RE.captures_iter(content).map(|c| c[1].to_string()));
    refs
}

fn extract_defs(content: &str) -> FxHashSet<String> {
    let mut defs = FxHashSet::default();
    defs.extend(
        MODULE_DEF_RE
            .captures_iter(content)
            .map(|c| c[1].to_string()),
    );
    defs.extend(LET_RE.captures_iter(content).map(|c| c[1].to_string()));
    defs.extend(VAL_RE.captures_iter(content).map(|c| c[1].to_string()));
    defs.extend(TYPE_DEF_RE.captures_iter(content).map(|c| c[1].to_string()));
    defs
}

fn extract_module_refs(content: &str) -> FxHashSet<String> {
    MODULE_REF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

pub struct OCamlEdgeBuilder;

impl EdgeBuilder for OCamlEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_ocaml_file(Path::new(f.path())))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let open_w = EDGE_WEIGHTS["ocaml_open"].forward;
        let _type_w = EDGE_WEIGHTS["ocaml_type"].forward;
        let fn_w = EDGE_WEIGHTS["ocaml_fn"].forward;
        let mod_w = EDGE_WEIGHTS["ocaml_module_ref"].forward;
        let reverse_factor = EDGE_WEIGHTS["ocaml_open"].reverse_factor;

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
            for open_name in extract_opens(&f.content) {
                base::link_by_name(&f.id, &open_name, &idx, &mut edges, open_w, reverse_factor);
                if let Some(targets) = name_to_defs.get(&open_name.to_lowercase()) {
                    add_edges_from_ids(&mut edges, &f.id, targets, open_w, reverse_factor);
                }
            }
            for mref in extract_module_refs(&f.content) {
                if self_defs.contains(&mref) {
                    continue;
                }
                if let Some(targets) = name_to_defs.get(&mref.to_lowercase()) {
                    for t in targets {
                        if t != &f.id {
                            add_edge(&mut edges, &f.id, t, mod_w, reverse_factor);
                        }
                    }
                }
            }
            for id in &f.identifiers {
                if self_defs.contains(id) {
                    continue;
                }
                if let Some(targets) = name_to_defs.get(&id.to_lowercase()) {
                    for t in targets {
                        if t != &f.id {
                            add_edge(&mut edges, &f.id, t, fn_w, reverse_factor);
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
        let ml_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_ocaml_file(f)).collect();
        if ml_changed.is_empty() {
            return vec![];
        }
        let mut refs = FxHashSet::default();
        for f in &ml_changed {
            if let Some(content) = base::read_file_cached(f, file_cache) {
                refs.extend(extract_opens(&content));
                refs.extend(extract_module_refs(&content));
            }
        }
        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
