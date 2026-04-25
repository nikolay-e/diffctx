use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::extensions::CODE_EXTENSIONS;
use crate::types::{Fragment, FragmentId};

use super::EdgeDict;

pub trait EdgeBuilder: Send + Sync {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict;

    fn discover_related_files(
        &self,
        _changed: &[PathBuf],
        _candidates: &[PathBuf],
        _repo_root: Option<&Path>,
        _file_cache: Option<&FxHashMap<PathBuf, String>>,
    ) -> Vec<PathBuf> {
        vec![]
    }

    fn category_label(&self) -> Option<&str> {
        None
    }

    fn is_expensive(&self) -> bool {
        false
    }
}

static INDEX_FILE_STEMS: Lazy<FxHashSet<&str>> =
    Lazy::new(|| ["__init__", "index", "mod"].iter().copied().collect());

fn strip_source_prefix(parts: &[&str]) -> Vec<String> {
    for (i, part) in parts.iter().enumerate() {
        if *part == "src" || *part == "lib" || *part == "packages" {
            return parts[i + 1..].iter().map(|s| s.to_string()).collect();
        }
    }
    parts.iter().map(|s| s.to_string()).collect()
}

fn strip_file_extension(stem: &str) -> &str {
    for ext in CODE_EXTENSIONS.iter() {
        if let Some(stripped) = stem.strip_suffix(ext) {
            return stripped;
        }
    }
    stem
}

pub fn path_to_module(path: &Path, repo_root: Option<&Path>) -> String {
    let effective = if let Some(root) = repo_root {
        if path.is_absolute() {
            path.strip_prefix(root).unwrap_or(path)
        } else {
            path
        }
    } else {
        path
    };

    let parts_raw: Vec<&str> = effective.iter().filter_map(|c| c.to_str()).collect();
    let mut parts = strip_source_prefix(&parts_raw);

    if let Some(last) = parts.last_mut() {
        let stripped = strip_file_extension(last).to_string();
        *last = stripped;
    }

    if let Some(last) = parts.last() {
        if INDEX_FILE_STEMS.contains(last.as_str()) {
            parts.pop();
        }
    }

    parts.join(".")
}

pub struct FragmentIndex {
    pub by_name: FxHashMap<String, Vec<FragmentId>>,
    pub by_path: FxHashMap<String, Vec<FragmentId>>,
}

impl FragmentIndex {
    pub fn new(fragments: &[Fragment], repo_root: Option<&Path>) -> Self {
        let mut by_name: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut by_path: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();

        for f in fragments {
            let path = Path::new(f.path());
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                by_name
                    .entry(name.to_lowercase())
                    .or_default()
                    .push(f.id.clone());
            }
            by_path
                .entry(f.path().to_string())
                .or_default()
                .push(f.id.clone());

            if let Some(root) = repo_root {
                if let Ok(rel) = Path::new(f.path()).strip_prefix(root) {
                    let rel_str = rel.to_string_lossy().to_string();
                    by_path
                        .entry(rel_str.clone())
                        .or_default()
                        .push(f.id.clone());
                    let posix = rel_str.replace('\\', "/");
                    if posix != rel_str {
                        by_path.entry(posix).or_default().push(f.id.clone());
                    }
                }
            }
        }

        Self { by_name, by_path }
    }
}

pub fn add_edge(
    edges: &mut EdgeDict,
    src: &FragmentId,
    dst: &FragmentId,
    weight: f64,
    reverse_factor: f64,
) {
    let key_fwd = (src.clone(), dst.clone());
    let existing_fwd = edges.get(&key_fwd).copied().unwrap_or(0.0);
    if weight > existing_fwd {
        edges.insert(key_fwd, weight);
    }
    let rev_w = weight * reverse_factor;
    let key_rev = (dst.clone(), src.clone());
    let existing_rev = edges.get(&key_rev).copied().unwrap_or(0.0);
    if rev_w > existing_rev {
        edges.insert(key_rev, rev_w);
    }
}

pub fn add_edge_unidirectional(
    edges: &mut EdgeDict,
    src: &FragmentId,
    dst: &FragmentId,
    weight: f64,
) {
    let key = (src.clone(), dst.clone());
    let existing = edges.get(&key).copied().unwrap_or(0.0);
    if weight > existing {
        edges.insert(key, weight);
    }
}

pub fn add_edges_from_ids(
    edges: &mut EdgeDict,
    src: &FragmentId,
    targets: &[FragmentId],
    weight: f64,
    reverse_factor: f64,
) {
    for target in targets {
        if target != src {
            add_edge(edges, src, target, weight, reverse_factor);
        }
    }
}

pub fn link_by_name(
    src_id: &FragmentId,
    name: &str,
    idx: &FragmentIndex,
    edges: &mut EdgeDict,
    weight: f64,
    reverse_factor: f64,
) {
    let target = name.split('/').next_back().unwrap_or(name).to_lowercase();
    if let Some(frag_ids) = idx.by_name.get(&target) {
        for fid in frag_ids {
            if fid != src_id {
                add_edge(edges, src_id, fid, weight, reverse_factor);
                return;
            }
        }
    }
    link_by_path_match(src_id, name, idx, edges, weight, reverse_factor);
}

pub fn link_by_path_match(
    src_id: &FragmentId,
    ref_str: &str,
    idx: &FragmentIndex,
    edges: &mut EdgeDict,
    weight: f64,
    reverse_factor: f64,
) {
    let ref_lower = ref_str.to_lowercase();
    for (path_str, frag_ids) in &idx.by_path {
        if path_str.contains(ref_str) || path_str.to_lowercase().contains(&ref_lower) {
            for fid in frag_ids {
                if fid != src_id {
                    add_edge(edges, src_id, fid, weight, reverse_factor);
                }
            }
        }
    }
}

pub fn read_file_cached<'a>(
    path: &Path,
    cache: Option<&'a FxHashMap<PathBuf, String>>,
) -> Option<String> {
    if let Some(c) = cache {
        if let Some(content) = c.get(path) {
            return Some(content.clone());
        }
    }
    std::fs::read_to_string(path).ok()
}

const MIN_REF_LENGTH_FOR_PATH_MATCH: usize = 3;

fn candidate_rel_path(candidate: &Path, repo_root: Option<&Path>) -> String {
    if let Some(root) = repo_root {
        if let Ok(rel) = candidate.strip_prefix(root) {
            return rel.to_string_lossy().to_lowercase();
        }
    }
    candidate
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn matches_any_ref(candidate_name: &str, candidate_rel: &str, refs: &FxHashSet<String>) -> bool {
    for r in refs {
        let ref_name = r.split('/').next_back().unwrap_or(r).to_lowercase();
        if candidate_name == ref_name {
            return true;
        }
        let ref_lower = r.to_lowercase();
        if ref_lower.len() >= MIN_REF_LENGTH_FOR_PATH_MATCH {
            if let Some(idx) = candidate_rel.find(&ref_lower) {
                let end_idx = idx + ref_lower.len();
                let start_ok = idx == 0
                    || candidate_rel.as_bytes().get(idx - 1) == Some(&b'/')
                    || candidate_rel.as_bytes().get(idx - 1) == Some(&b'\\');
                let end_ok = end_idx == candidate_rel.len()
                    || matches!(
                        candidate_rel.as_bytes().get(end_idx),
                        Some(b'/') | Some(b'\\') | Some(b'.')
                    );
                if start_ok && end_ok {
                    return true;
                }
            }
        }
    }
    false
}

pub fn discover_files_by_refs(
    refs: &FxHashSet<String>,
    changed_files: &[PathBuf],
    all_candidates: &[PathBuf],
    repo_root: Option<&Path>,
) -> Vec<PathBuf> {
    if refs.is_empty() {
        return vec![];
    }
    let changed_set: FxHashSet<&PathBuf> = changed_files.iter().collect();
    let mut discovered = Vec::new();
    for candidate in all_candidates {
        if changed_set.contains(candidate) {
            continue;
        }
        let candidate_name = candidate
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let candidate_rel = candidate_rel_path(candidate, repo_root);
        if matches_any_ref(&candidate_name, &candidate_rel, refs) {
            discovered.push(candidate.clone());
        }
    }
    discovered
}

pub fn file_ext(path: &Path) -> String {
    path.extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default()
}
