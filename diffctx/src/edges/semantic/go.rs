use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::edge_weights::{GO_SEMANTIC, SEMANTIC_DISCOVERY};
use crate::config::extensions::GO_EXTENSIONS;
use crate::config::weights::EDGE_WEIGHTS;
use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, add_edges_from_ids};

fn is_go_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    GO_EXTENSIONS.contains(ext.as_str())
}

static IMPORT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^\s*(?:import\s+)?"([^"]+)""#).unwrap());
static PACKAGE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*package\s+(\w+)").unwrap());
static TYPE_DEF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*type\s+([A-Z]\w*)").unwrap());
static FUNC_DEF_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*func\s+(?:\([^)]*\)\s+)?([A-Z]\w*)\s*\(").unwrap());
static FUNC_CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w*)\s*\(").unwrap());
static TYPE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w*)\b").unwrap());
static PKG_CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([a-z]\w+)\.([A-Z]\w*)").unwrap());
static INIT_FUNC_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*func\s+init\s*\(").unwrap());

fn extract_imports(content: &str) -> FxHashSet<String> {
    IMPORT_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn get_package_name(content: &str) -> String {
    PACKAGE_RE
        .captures(content)
        .map(|c| c[1].to_string())
        .unwrap_or_else(|| "main".to_string())
}

fn extract_definitions(content: &str) -> (FxHashSet<String>, FxHashSet<String>) {
    let funcs: FxHashSet<String> = FUNC_DEF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect();
    let types: FxHashSet<String> = TYPE_DEF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect();
    (funcs, types)
}

fn extract_references(
    content: &str,
) -> (
    FxHashSet<String>,
    FxHashSet<String>,
    FxHashSet<(String, String)>,
) {
    let func_calls: FxHashSet<String> = FUNC_CALL_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect();
    let type_refs: FxHashSet<String> = TYPE_REF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect();
    let pkg_calls: FxHashSet<(String, String)> = PKG_CALL_RE
        .captures_iter(content)
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .collect();
    (func_calls, type_refs, pkg_calls)
}

fn has_init_func(content: &str) -> bool {
    INIT_FUNC_RE.is_match(content)
}

pub struct GoEdgeBuilder;

impl GoEdgeBuilder {
    fn build_indices(
        &self,
        go_frags: &[&Fragment],
        repo_root: Option<&Path>,
    ) -> (
        FxHashMap<String, Vec<FragmentId>>,
        FxHashMap<String, Vec<FragmentId>>,
        FxHashMap<String, Vec<FragmentId>>,
        FxHashMap<String, Vec<FragmentId>>,
    ) {
        let mut pkg_to_frags: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut path_to_frags: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut type_defs: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut func_defs: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();

        for f in go_frags {
            let pkg = get_package_name(&f.content).to_lowercase();
            pkg_to_frags.entry(pkg).or_default().push(f.id.clone());

            if let Some(root) = repo_root {
                if let Ok(rel) = Path::new(f.path()).strip_prefix(root) {
                    if let Some(parent) = rel.parent() {
                        path_to_frags
                            .entry(parent.to_string_lossy().to_string())
                            .or_default()
                            .push(f.id.clone());
                    }
                }
            }

            let (funcs, types) = extract_definitions(&f.content);
            for t in types {
                type_defs
                    .entry(t.to_lowercase())
                    .or_default()
                    .push(f.id.clone());
            }
            for func in funcs {
                func_defs
                    .entry(func.to_lowercase())
                    .or_default()
                    .push(f.id.clone());
            }
        }

        (pkg_to_frags, path_to_frags, type_defs, func_defs)
    }
}

impl EdgeBuilder for GoEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let go_frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_go_file(Path::new(f.path())))
            .collect();
        if go_frags.is_empty() {
            return FxHashMap::default();
        }

        let import_weight = EDGE_WEIGHTS["go_import"].forward;
        let type_weight = EDGE_WEIGHTS["go_type"].forward;
        let func_weight = EDGE_WEIGHTS["go_func"].forward;
        let same_package_weight = EDGE_WEIGHTS["go_same_package"].forward;
        let reverse_factor = EDGE_WEIGHTS["go_import"].reverse_factor;
        let init_same_package_weight = GO_SEMANTIC.init_same_package_weight;

        let (pkg_to_frags, path_to_frags, type_defs, func_defs) =
            self.build_indices(&go_frags, repo_root);

        let mut edges: EdgeDict = FxHashMap::default();

        for gf in &go_frags {
            let imports = extract_imports(&gf.content);
            let (func_calls, type_refs, pkg_calls) = extract_references(&gf.content);

            for imp in &imports {
                let imp_pkg = imp.split('/').next_back().unwrap_or(imp).to_lowercase();
                for (pkg, frag_ids) in &pkg_to_frags {
                    if *pkg == imp_pkg {
                        add_edges_from_ids(
                            &mut edges,
                            &gf.id,
                            frag_ids,
                            import_weight,
                            reverse_factor,
                        );
                    }
                }
                for (path_str, frag_ids) in &path_to_frags {
                    if *imp == *path_str
                        || imp.ends_with(&format!("/{}", path_str))
                        || imp.contains(&format!("/{}/", path_str))
                    {
                        add_edges_from_ids(
                            &mut edges,
                            &gf.id,
                            frag_ids,
                            import_weight,
                            reverse_factor,
                        );
                    }
                }
            }

            for type_ref in &type_refs {
                for fid in type_defs.get(&type_ref.to_lowercase()).unwrap_or(&vec![]) {
                    if fid != &gf.id {
                        add_edge(&mut edges, &gf.id, fid, type_weight, reverse_factor);
                    }
                }
            }

            for func_call in &func_calls {
                for fid in func_defs.get(&func_call.to_lowercase()).unwrap_or(&vec![]) {
                    if fid != &gf.id {
                        add_edge(&mut edges, &gf.id, fid, func_weight, reverse_factor);
                    }
                }
            }

            for (pkg_name, _symbol) in &pkg_calls {
                for fid in pkg_to_frags
                    .get(&pkg_name.to_lowercase())
                    .unwrap_or(&vec![])
                {
                    if fid != &gf.id {
                        add_edge(&mut edges, &gf.id, fid, func_weight, reverse_factor);
                    }
                }
            }

            let has_init = has_init_func(&gf.content);
            let sp_weight = if has_init {
                init_same_package_weight
            } else {
                same_package_weight
            };
            let current_pkg = get_package_name(&gf.content).to_lowercase();
            for fid in pkg_to_frags.get(&current_pkg).unwrap_or(&vec![]) {
                if fid != &gf.id {
                    add_edge(&mut edges, &gf.id, fid, sp_weight, reverse_factor);
                }
            }
        }

        edges
    }

    fn discover_related_files(
        &self,
        changed: &[PathBuf],
        candidates: &[PathBuf],
        _repo_root: Option<&Path>,
        file_cache: Option<&FxHashMap<PathBuf, String>>,
    ) -> Vec<PathBuf> {
        let go_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_go_file(f)).collect();
        if go_changed.is_empty() {
            return vec![];
        }

        let changed_set: FxHashSet<PathBuf> = changed.iter().cloned().collect();
        let go_candidates: Vec<PathBuf> = candidates
            .iter()
            .filter(|c| !changed_set.contains(*c) && is_go_file(c))
            .cloned()
            .collect();

        let mut discovered: FxHashSet<PathBuf> = FxHashSet::default();

        let pkg_dirs: FxHashSet<PathBuf> = go_changed
            .iter()
            .filter_map(|f| f.parent().map(|p| p.to_path_buf()))
            .collect();
        for c in &go_candidates {
            if let Some(parent) = c.parent() {
                if pkg_dirs.contains(&parent.to_path_buf()) {
                    discovered.insert(c.clone());
                }
            }
        }

        let mut candidate_index: FxHashMap<PathBuf, (String, FxHashSet<String>)> =
            FxHashMap::default();
        for c in &go_candidates {
            let content = base::read_file_cached(c, file_cache);
            if let Some(content) = content {
                let pkg = get_package_name(&content).to_lowercase();
                let imports = extract_imports(&content);
                candidate_index.insert(c.clone(), (pkg, imports));
            }
        }

        let mut frontier: FxHashSet<PathBuf> = go_changed.iter().map(|f| (*f).clone()).collect();

        for _ in 0..SEMANTIC_DISCOVERY.max_depth {
            let mut next_frontier: FxHashSet<PathBuf> = FxHashSet::default();
            for f in &frontier {
                let content = base::read_file_cached(f, file_cache);
                if let Some(content) = content {
                    let f_imports = extract_imports(&content);
                    let f_pkg = get_package_name(&content).to_lowercase();

                    for c in &go_candidates {
                        if changed_set.contains(c) || discovered.contains(c) {
                            continue;
                        }
                        if let Some((c_pkg, c_imports)) = candidate_index.get(c) {
                            let forward_match = f_imports.iter().any(|imp| {
                                imp.split('/').next_back().unwrap_or(imp).to_lowercase() == *c_pkg
                            });
                            let reverse_match = c_imports.iter().any(|imp| {
                                imp.split('/').next_back().unwrap_or(imp).to_lowercase() == f_pkg
                            });
                            if forward_match || reverse_match {
                                discovered.insert(c.clone());
                                next_frontier.insert(c.clone());
                            }
                        }
                    }
                }
            }
            if next_frontier.is_empty() {
                break;
            }
            frontier = next_frontier;
        }

        let mut result: Vec<PathBuf> = discovered.into_iter().collect();
        result.sort();
        result
    }
}
