use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, discover_files_by_refs};

fn is_lua_file(path: &Path) -> bool {
    base::file_ext(path) == ".lua"
}

static REQUIRE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)require\s*[\(]?\s*['"]([^'"]+)['"]"#).unwrap());
static DOFILE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)dofile\s*\(\s*['"]([^'"]+)['"]"#).unwrap());
static FUNC_DEF_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*(?:local\s+)?function\s+([\w.:]+)").unwrap());
static LOCAL_FUNC_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*local\s+(\w+)\s*=\s*function").unwrap());
static METHOD_CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b(\w+)[:.]\w+\s*\(").unwrap());

fn extract_requires(content: &str) -> FxHashSet<String> {
    let mut refs: FxHashSet<String> = REQUIRE_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect();
    refs.extend(DOFILE_RE.captures_iter(content).map(|c| c[1].to_string()));
    refs
}

fn extract_defs(content: &str) -> FxHashSet<String> {
    let mut defs: FxHashSet<String> = FxHashSet::default();
    for cap in FUNC_DEF_RE.captures_iter(content) {
        let name = &cap[1];
        let leaf = name.split(&['.', ':'][..]).last().unwrap_or(name);
        defs.insert(leaf.to_string());
    }
    defs.extend(
        LOCAL_FUNC_RE
            .captures_iter(content)
            .map(|c| c[1].to_string()),
    );
    defs
}

fn extract_method_targets(content: &str) -> FxHashSet<String> {
    METHOD_CALL_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

pub struct LuaEdgeBuilder;

impl EdgeBuilder for LuaEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_lua_file(Path::new(f.path())))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let require_w = EDGE_WEIGHTS["lua_require"].forward;
        let fn_w = EDGE_WEIGHTS["lua_fn"].forward;
        let method_w = EDGE_WEIGHTS["lua_method"].forward;
        let reverse_factor = EDGE_WEIGHTS["lua_require"].reverse_factor;

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
            for req in extract_requires(&f.content) {
                base::link_by_name(&f.id, &req, &idx, &mut edges, require_w, reverse_factor);
            }
            for target in extract_method_targets(&f.content) {
                if self_defs.contains(&target) {
                    continue;
                }
                if let Some(targets) = name_to_defs.get(&target.to_lowercase()) {
                    for t in targets {
                        if t != &f.id {
                            add_edge(&mut edges, &f.id, t, method_w, reverse_factor);
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
        let lua_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_lua_file(f)).collect();
        if lua_changed.is_empty() {
            return vec![];
        }
        let mut refs = FxHashSet::default();
        for f in &lua_changed {
            if let Some(content) = base::read_file_cached(f, file_cache) {
                refs.extend(extract_requires(&content));
            }
        }
        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
