use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, add_edges_from_ids};

const WEIGHT: f64 = 0.60;
const REVERSE_FACTOR: f64 = 0.40;
const MODULE_SOURCE_WEIGHT: f64 = WEIGHT * 0.8;

static TF_EXTENSIONS: Lazy<FxHashSet<&str>> =
    Lazy::new(|| [".tf", ".tfvars", ".hcl"].iter().copied().collect());

fn is_terraform_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    TF_EXTENSIONS.contains(ext.as_str())
}

static VARIABLE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?m)^variable\s+"([^"]+)""#).unwrap());
static RESOURCE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^resource\s+"([^"]+)"\s+"([^"]+)""#).unwrap());
static DATA_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^data\s+"([^"]+)"\s+"([^"]+)""#).unwrap());
static MODULE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?m)^module\s+"([^"]+)""#).unwrap());
static LOCALS_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^locals\s*\{").unwrap());
static LOCAL_KEY_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s+(\w+)\s*=").unwrap());

static VAR_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"var\.(\w+)").unwrap());
static LOCAL_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"local\.(\w+)").unwrap());
static DATA_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"data\.(\w+)\.(\w+)").unwrap());
static RESOURCE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)(\w+)\.(\w+)\.(\w+)").unwrap());
static MODULE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"module\.(\w+)").unwrap());

static SOURCE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^\s*source\s*=\s*"([^"]+)""#).unwrap());

static GENERIC_NAMES: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [
        "name",
        "region",
        "tags",
        "environment",
        "env",
        "description",
        "enabled",
        "type",
        "value",
        "default",
        "count",
        "id",
        "arn",
        "vpc_id",
        "subnet_id",
        "key",
        "project",
        "owner",
        "stage",
    ]
    .iter()
    .copied()
    .collect()
});

static RESOURCE_SKIP_TYPES: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [
        "var",
        "local",
        "data",
        "module",
        "path",
        "terraform",
        "each",
        "self",
        "count",
    ]
    .iter()
    .copied()
    .collect()
});

fn extract_locals(content: &str) -> FxHashSet<String> {
    let mut locals_keys = FxHashSet::default();
    let mut in_locals = false;
    let mut brace_count: i32 = 0;

    for line in content.lines() {
        if LOCALS_RE.is_match(line) {
            in_locals = true;
            brace_count = line.matches('{').count() as i32 - line.matches('}').count() as i32;
            if brace_count <= 0 {
                in_locals = false;
            }
            continue;
        }

        if in_locals {
            brace_count += line.matches('{').count() as i32 - line.matches('}').count() as i32;
            if brace_count <= 0 {
                in_locals = false;
                continue;
            }

            if let Some(cap) = LOCAL_KEY_RE.captures(line) {
                locals_keys.insert(cap[1].to_string());
            }
        }
    }

    locals_keys
}

fn extract_qualified_defs(content: &str) -> FxHashSet<String> {
    let mut defs = FxHashSet::default();

    for cap in VARIABLE_RE.captures_iter(content) {
        defs.insert(cap[1].to_string());
    }
    for cap in RESOURCE_RE.captures_iter(content) {
        defs.insert(format!("{}.{}", &cap[1], &cap[2]));
    }
    for cap in DATA_RE.captures_iter(content) {
        defs.insert(format!("{}.{}", &cap[1], &cap[2]));
    }
    for local_key in extract_locals(content) {
        defs.insert(local_key);
    }
    for cap in MODULE_RE.captures_iter(content) {
        defs.insert(cap[1].to_string());
    }

    defs
}

fn has_non_generic_var_local_ref(content: &str, changed_defs: &FxHashSet<String>) -> bool {
    for cap in VAR_REF_RE.captures_iter(content) {
        let name = &cap[1];
        if changed_defs.contains(name) && !GENERIC_NAMES.contains(name) {
            return true;
        }
    }
    for cap in LOCAL_REF_RE.captures_iter(content) {
        let name = &cap[1];
        if changed_defs.contains(name) && !GENERIC_NAMES.contains(name) {
            return true;
        }
    }
    false
}

fn has_data_module_resource_ref(content: &str, changed_defs: &FxHashSet<String>) -> bool {
    for cap in DATA_REF_RE.captures_iter(content) {
        let full = format!("{}.{}", &cap[1], &cap[2]);
        if changed_defs.contains(&full) || changed_defs.contains(&cap[2].to_string()) {
            return true;
        }
    }
    for cap in MODULE_REF_RE.captures_iter(content) {
        if changed_defs.contains(&cap[1].to_string()) {
            return true;
        }
    }
    for cap in RESOURCE_REF_RE.captures_iter(content) {
        let res_type = &cap[1];
        if RESOURCE_SKIP_TYPES.contains(res_type) {
            continue;
        }
        let res_name = &cap[2];
        let full = format!("{}.{}", res_type, res_name);
        if changed_defs.contains(&full) || changed_defs.contains(&res_name.to_string()) {
            return true;
        }
    }
    false
}

fn candidate_references_changed_defs_strict(
    content: &str,
    changed_defs: &FxHashSet<String>,
) -> bool {
    has_non_generic_var_local_ref(content, changed_defs)
        || has_data_module_resource_ref(content, changed_defs)
}

fn collect_tf_dirs_and_sources(
    tf_files: &[&PathBuf],
    file_cache: Option<&FxHashMap<PathBuf, String>>,
) -> (FxHashSet<PathBuf>, FxHashSet<String>) {
    let mut tf_dirs = FxHashSet::default();
    let mut module_sources = FxHashSet::default();

    for tf in tf_files {
        if let Some(parent) = tf.parent() {
            tf_dirs.insert(parent.to_path_buf());
        }
        if let Some(content) = base::read_file_cached(tf, file_cache) {
            for cap in SOURCE_RE.captures_iter(&content) {
                let src = &cap[1];
                if src.starts_with("./") || src.starts_with("../") {
                    module_sources.insert(src.to_string());
                }
            }
        }
    }

    (tf_dirs, module_sources)
}

fn resolve_module_paths(
    src: &str,
    tf_dirs: &FxHashSet<PathBuf>,
    repo_root: Option<&Path>,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for tf_dir in tf_dirs {
        if let Ok(resolved) = tf_dir.join(src).canonicalize() {
            paths.push(resolved);
        }
    }
    if let Some(root) = repo_root {
        let stripped = src.trim_start_matches("./");
        if let Ok(resolved) = root.join(stripped).canonicalize() {
            paths.push(resolved);
        }
    }
    paths
}

fn is_in_module(
    candidate: &Path,
    module_sources: &FxHashSet<String>,
    tf_dirs: &FxHashSet<PathBuf>,
    repo_root: Option<&Path>,
) -> bool {
    for src in module_sources {
        for module_path in resolve_module_paths(src, tf_dirs, repo_root) {
            if candidate.starts_with(&module_path) {
                return true;
            }
        }
    }
    false
}

struct TFIndex {
    var_defs: FxHashMap<String, Vec<FragmentId>>,
    resource_defs: FxHashMap<String, Vec<FragmentId>>,
    data_defs: FxHashMap<String, Vec<FragmentId>>,
    local_defs: FxHashMap<String, Vec<FragmentId>>,
    module_defs: FxHashMap<String, Vec<FragmentId>>,
}

impl TFIndex {
    fn new() -> Self {
        Self {
            var_defs: FxHashMap::default(),
            resource_defs: FxHashMap::default(),
            data_defs: FxHashMap::default(),
            local_defs: FxHashMap::default(),
            module_defs: FxHashMap::default(),
        }
    }
}

fn index_definitions(f: &Fragment, idx: &mut TFIndex) {
    for cap in VARIABLE_RE.captures_iter(&f.content) {
        idx.var_defs
            .entry(cap[1].to_string())
            .or_default()
            .push(f.id.clone());
    }

    for cap in RESOURCE_RE.captures_iter(&f.content) {
        let full = format!("{}.{}", &cap[1], &cap[2]);
        let name = cap[2].to_string();
        idx.resource_defs
            .entry(full)
            .or_default()
            .push(f.id.clone());
        idx.resource_defs
            .entry(name)
            .or_default()
            .push(f.id.clone());
    }

    for cap in DATA_RE.captures_iter(&f.content) {
        let full = format!("{}.{}", &cap[1], &cap[2]);
        let name = cap[2].to_string();
        idx.data_defs.entry(full).or_default().push(f.id.clone());
        idx.data_defs.entry(name).or_default().push(f.id.clone());
    }

    for local_key in extract_locals(&f.content) {
        idx.local_defs
            .entry(local_key)
            .or_default()
            .push(f.id.clone());
    }

    for cap in MODULE_RE.captures_iter(&f.content) {
        idx.module_defs
            .entry(cap[1].to_string())
            .or_default()
            .push(f.id.clone());
    }
}

fn build_index(tf_frags: &[&Fragment]) -> TFIndex {
    let mut idx = TFIndex::new();
    for f in tf_frags {
        index_definitions(f, &mut idx);
    }
    idx
}

fn add_var_edges(f: &Fragment, idx: &TFIndex, edges: &mut EdgeDict) {
    for cap in VAR_REF_RE.captures_iter(&f.content) {
        if let Some(def_ids) = idx.var_defs.get(&cap[1].to_string()) {
            add_edges_from_ids(edges, &f.id, def_ids, WEIGHT, REVERSE_FACTOR);
        }
    }
}

fn add_local_edges(f: &Fragment, idx: &TFIndex, edges: &mut EdgeDict) {
    for cap in LOCAL_REF_RE.captures_iter(&f.content) {
        if let Some(def_ids) = idx.local_defs.get(&cap[1].to_string()) {
            add_edges_from_ids(edges, &f.id, def_ids, WEIGHT, REVERSE_FACTOR);
        }
    }
}

fn add_data_edges(f: &Fragment, idx: &TFIndex, edges: &mut EdgeDict) {
    for cap in DATA_REF_RE.captures_iter(&f.content) {
        let full = format!("{}.{}", &cap[1], &cap[2]);
        let name = cap[2].to_string();
        if let Some(def_ids) = idx.data_defs.get(&full) {
            add_edges_from_ids(edges, &f.id, def_ids, WEIGHT, REVERSE_FACTOR);
        }
        if let Some(def_ids) = idx.data_defs.get(&name) {
            add_edges_from_ids(edges, &f.id, def_ids, WEIGHT, REVERSE_FACTOR);
        }
    }
}

fn add_module_edges(f: &Fragment, idx: &TFIndex, edges: &mut EdgeDict) {
    for cap in MODULE_REF_RE.captures_iter(&f.content) {
        if let Some(def_ids) = idx.module_defs.get(&cap[1].to_string()) {
            add_edges_from_ids(edges, &f.id, def_ids, WEIGHT, REVERSE_FACTOR);
        }
    }
}

fn add_resource_edges(f: &Fragment, idx: &TFIndex, edges: &mut EdgeDict) {
    for cap in RESOURCE_REF_RE.captures_iter(&f.content) {
        let res_type = &cap[1];
        if RESOURCE_SKIP_TYPES.contains(res_type) {
            continue;
        }
        let res_name = &cap[2];
        let full = format!("{}.{}", res_type, res_name);
        if let Some(def_ids) = idx.resource_defs.get(&full) {
            add_edges_from_ids(edges, &f.id, def_ids, WEIGHT, REVERSE_FACTOR);
        }
        if let Some(def_ids) = idx.resource_defs.get(&res_name.to_string()) {
            add_edges_from_ids(edges, &f.id, def_ids, WEIGHT, REVERSE_FACTOR);
        }
    }
}

fn add_ref_edges(f: &Fragment, idx: &TFIndex, edges: &mut EdgeDict) {
    add_var_edges(f, idx, edges);
    add_local_edges(f, idx, edges);
    add_data_edges(f, idx, edges);
    add_module_edges(f, idx, edges);
    add_resource_edges(f, idx, edges);
}

fn build_path_to_frags(
    all_frags: &[Fragment],
    repo_root: Option<&Path>,
) -> FxHashMap<PathBuf, Vec<FragmentId>> {
    let mut map: FxHashMap<PathBuf, Vec<FragmentId>> = FxHashMap::default();
    for f in all_frags {
        let path = PathBuf::from(f.path());
        map.entry(path.clone()).or_default().push(f.id.clone());
        if let Some(root) = repo_root {
            if let Ok(rel) = path.strip_prefix(root) {
                map.entry(rel.to_path_buf()).or_default().push(f.id.clone());
            }
        }
    }
    map
}

fn build_module_source_edges(
    tf_frags: &[&Fragment],
    all_frags: &[Fragment],
    edges: &mut EdgeDict,
    repo_root: Option<&Path>,
) {
    let path_to_frags = build_path_to_frags(all_frags, repo_root);

    for f in tf_frags {
        let base_dir = Path::new(f.path()).parent().unwrap_or(Path::new(""));

        for cap in SOURCE_RE.captures_iter(&f.content) {
            let source = &cap[1];
            if source.starts_with("./") || source.starts_with("../") {
                let module_dir = base_dir.join(source);
                let resolved = module_dir.canonicalize().unwrap_or(module_dir);

                for (p, frag_ids) in &path_to_frags {
                    let candidate = if p.is_absolute() {
                        p.clone()
                    } else if let Some(root) = repo_root {
                        root.join(p).canonicalize().unwrap_or_else(|_| root.join(p))
                    } else {
                        p.clone()
                    };
                    if candidate.starts_with(&resolved) {
                        for frag_id in frag_ids {
                            if *frag_id != f.id {
                                add_edge(
                                    edges,
                                    &f.id,
                                    frag_id,
                                    MODULE_SOURCE_WEIGHT,
                                    REVERSE_FACTOR,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

pub struct TerraformEdgeBuilder;

impl EdgeBuilder for TerraformEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let tf_frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_terraform_file(Path::new(f.path())))
            .collect();
        if tf_frags.is_empty() {
            return FxHashMap::default();
        }

        let mut edges: EdgeDict = FxHashMap::default();
        let idx = build_index(&tf_frags);

        for f in &tf_frags {
            add_ref_edges(f, &idx, &mut edges);
        }

        build_module_source_edges(&tf_frags, fragments, &mut edges, repo_root);

        edges
    }

    fn discover_related_files(
        &self,
        changed: &[PathBuf],
        candidates: &[PathBuf],
        repo_root: Option<&Path>,
        file_cache: Option<&FxHashMap<PathBuf, String>>,
    ) -> Vec<PathBuf> {
        let tf_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_terraform_file(f)).collect();
        if tf_changed.is_empty() {
            return vec![];
        }

        let (tf_dirs, module_sources) = collect_tf_dirs_and_sources(&tf_changed, file_cache);

        let mut changed_defs = FxHashSet::default();
        let mut changed_contents: Vec<String> = Vec::new();
        for tf in &tf_changed {
            if let Some(content) = base::read_file_cached(tf, file_cache) {
                for def in extract_qualified_defs(&content) {
                    changed_defs.insert(def);
                }
                changed_contents.push(content);
            }
        }

        let changed_set: FxHashSet<&PathBuf> = changed.iter().collect();
        let mut result = Vec::new();

        for c in candidates {
            if changed_set.contains(c) || !is_terraform_file(c) {
                continue;
            }
            if is_related(
                c,
                &module_sources,
                &tf_dirs,
                repo_root,
                &changed_defs,
                &changed_contents,
                file_cache,
            ) {
                result.push(c.clone());
            }
        }

        result.sort();
        result
    }

    fn category_label(&self) -> Option<&str> {
        Some("semantic")
    }
}

fn is_related(
    candidate: &Path,
    module_sources: &FxHashSet<String>,
    tf_dirs: &FxHashSet<PathBuf>,
    repo_root: Option<&Path>,
    changed_defs: &FxHashSet<String>,
    changed_contents: &[String],
    file_cache: Option<&FxHashMap<PathBuf, String>>,
) -> bool {
    if is_in_module(candidate, module_sources, tf_dirs, repo_root) {
        return true;
    }

    let candidate_parent = candidate.parent().map(|p| p.to_path_buf());
    if let Some(parent) = &candidate_parent {
        if !tf_dirs.contains(parent) {
            return false;
        }
    } else {
        return false;
    }

    let content = match base::read_file_cached(candidate, file_cache) {
        Some(c) => c,
        None => return false,
    };

    if candidate_references_changed_defs_strict(&content, changed_defs) {
        return true;
    }

    let candidate_defs = extract_qualified_defs(&content);
    !candidate_defs.is_empty()
        && changed_contents
            .iter()
            .any(|c| candidate_references_changed_defs_strict(c, &candidate_defs))
}
