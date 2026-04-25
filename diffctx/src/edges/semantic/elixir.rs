use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, add_edges_from_ids, discover_files_by_refs};

fn is_elixir_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    ext == ".ex" || ext == ".exs"
}

static ALIAS_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*alias\s+([\w.]+)").unwrap());
static IMPORT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*import\s+([\w.]+)").unwrap());
static USE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*use\s+([\w.]+)").unwrap());
static REQUIRE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*require\s+([\w.]+)").unwrap());
static DEFMODULE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*defmodule\s+([\w.]+)").unwrap());
static DEF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:def|defp|defmacro|defmacrop|defguard|defdelegate)\s+([a-z_]\w*)")
        .unwrap()
});
static BEHAVIOUR_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*@behaviour\s+([\w.]+)").unwrap());
static MODULE_REF_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b([A-Z]\w*(?:\.[A-Z]\w*)*)\b").unwrap());

fn extract_refs(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();
    for re in [
        &*ALIAS_RE,
        &*IMPORT_RE,
        &*USE_RE,
        &*REQUIRE_RE,
        &*BEHAVIOUR_RE,
    ] {
        refs.extend(re.captures_iter(content).map(|c| c[1].to_string()));
    }
    refs
}

fn extract_module_defs(content: &str) -> FxHashSet<String> {
    DEFMODULE_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_func_defs(content: &str) -> FxHashSet<String> {
    DEF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

pub struct ElixirEdgeBuilder;

impl EdgeBuilder for ElixirEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_elixir_file(Path::new(f.path())))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let use_w = EDGE_WEIGHTS["elixir_use"].forward;
        let alias_w = EDGE_WEIGHTS["elixir_alias"].forward;
        let behaviour_w = EDGE_WEIGHTS["elixir_behaviour"].forward;
        let fn_w = EDGE_WEIGHTS["elixir_fn"].forward;
        let reverse_factor = EDGE_WEIGHTS["elixir_use"].reverse_factor;

        let mut module_to_frags: FxHashMap<String, Vec<_>> = FxHashMap::default();
        let mut fn_to_frags: FxHashMap<String, Vec<_>> = FxHashMap::default();
        for f in &frags {
            for m in extract_module_defs(&f.content) {
                let leaf = m.split('.').last().unwrap_or(&m).to_lowercase();
                module_to_frags.entry(leaf).or_default().push(f.id.clone());
                module_to_frags
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
            let refs = extract_refs(&f.content);
            for r in &refs {
                let leaf = r.split('.').last().unwrap_or(r).to_lowercase();
                let full = r.to_lowercase();
                let w = if BEHAVIOUR_RE.is_match(&format!("@behaviour {}", r)) {
                    behaviour_w
                } else if USE_RE.is_match(&format!("use {}", r)) {
                    use_w
                } else {
                    alias_w
                };
                for key in [&leaf, &full] {
                    if let Some(targets) = module_to_frags.get(key) {
                        add_edges_from_ids(&mut edges, &f.id, targets, w, reverse_factor);
                    }
                }
            }
            for mref in MODULE_REF_RE.captures_iter(&f.content) {
                let name = &mref[1];
                let leaf = name.split('.').last().unwrap_or(name).to_lowercase();
                if let Some(targets) = module_to_frags.get(&leaf) {
                    for t in targets {
                        if t != &f.id {
                            add_edge(&mut edges, &f.id, t, fn_w, reverse_factor);
                        }
                    }
                }
            }
            for id in &f.identifiers {
                if self_fns.contains(id) {
                    continue;
                }
                if let Some(targets) = fn_to_frags.get(&id.to_lowercase()) {
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
        let ex_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_elixir_file(f)).collect();
        if ex_changed.is_empty() {
            return vec![];
        }
        let mut refs = FxHashSet::default();
        for f in &ex_changed {
            if let Some(content) = base::read_file_cached(f, file_cache) {
                refs.extend(extract_refs(&content));
            }
        }
        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
