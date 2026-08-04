use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::edge_weights::SEMANTIC_DISCOVERY;
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
    /// Lowercased path table plus a component→paths posting list, so a path
    /// reference resolves by looking up its last component instead of scanning
    /// every indexed path. Every accepted alignment (equal, suffix, prefix,
    /// interior) requires each reference component to appear as a whole path
    /// component, so the last component is a complete candidate filter. Each
    /// entry carries the file's representative fragment: a path reference is a
    /// file-level relation, and the containment star spreads its mass inside
    /// the file.
    lower_paths: Vec<(String, FragmentId)>,
    component_to_paths: FxHashMap<String, Vec<u32>>,
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

        let reps = file_representatives(fragments);
        let mut lower_paths: Vec<(String, FragmentId)> = Vec::with_capacity(by_path.len());
        let mut component_to_paths: FxHashMap<String, Vec<u32>> = FxHashMap::default();
        for (path_str, ids) in &by_path {
            // Every id under one path key belongs to one file; its
            // representative is looked up by the id's own (canonical) path,
            // which also covers the relative-variant keys.
            let Some(rep) = ids.first().and_then(|id| reps.get(id.path.as_ref())) else {
                continue;
            };
            let idx = lower_paths.len() as u32;
            let lower = path_str.to_lowercase();
            for comp in lower.split('/').filter(|c| !c.is_empty()) {
                let posting = component_to_paths.entry(comp.to_string()).or_default();
                if posting.last() != Some(&idx) {
                    posting.push(idx);
                }
            }
            lower_paths.push((lower, rep.clone()));
        }

        Self {
            by_name,
            by_path,
            lower_paths,
            component_to_paths,
        }
    }
}

/// One representative fragment per file — the largest by token count, ties
/// resolved by first-seen order. This is the semantics `SiblingEdgeBuilder`
/// always used for its file-level edges; file-level *relations* (an include,
/// a header/impl pair, a path reference) link representatives rather than
/// every-fragment-to-every-fragment, because the relation names the file. A
/// change endorsing a file diffusely must not outweigh a call edge naming one
/// symbol — the fragment-pair encoding gave both the same weight, which is
/// simultaneously the quadratic edge blow-up (envoy: 22M c-family edges) and
/// the file-level noise mechanism of #65. Reachability of the file's other
/// fragments is the containment star's job.
pub fn file_representatives(fragments: &[Fragment]) -> FxHashMap<String, FragmentId> {
    let mut file_to_rep: FxHashMap<String, FragmentId> = FxHashMap::default();
    let mut file_to_token_count: FxHashMap<String, u32> = FxHashMap::default();

    for f in fragments {
        let path = f.path().to_string();
        let existing_count = file_to_token_count.get(&path).copied().unwrap_or(0);
        if !file_to_rep.contains_key(&path) || f.token_count > existing_count {
            file_to_rep.insert(path.clone(), f.id.clone());
            file_to_token_count.insert(path, f.token_count);
        }
    }

    file_to_rep
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

/// Links `src_id` to fragments of files the reference plausibly names.
///
/// A match must be component-aligned: the reference equals the path, a full
/// suffix of it, or a full prefix (a bazel package label names a directory).
/// The previous `contains` accepted a match *anywhere* in the path, so a short
/// generic reference — bazel deps and ansible roles are full of `config`,
/// `test`, `common` — matched thousands of paths and linked every fragment of
/// each, which is one of the two mechanisms behind envoy-class instances
/// hanging the graph build (50% of dcbench).
pub fn link_by_path_match(
    src_id: &FragmentId,
    ref_str: &str,
    idx: &FragmentIndex,
    edges: &mut EdgeDict,
    weight: f64,
    reverse_factor: f64,
) {
    let ref_norm = ref_str.trim_matches('/');
    if ref_norm.is_empty() {
        return;
    }
    // Case folding subsumes the exact comparison, so matching is done entirely
    // in lowercase. The last reference component narrows the scan: it must be
    // a whole component of any aligned path, so only its posting list is
    // visited instead of every indexed path — the full scan was where an
    // envoy-scale bazel/ansible pass burned its 240s (~1e5 refs x ~1e4 paths,
    // an allocation per probe).
    let ref_lower = ref_norm.to_lowercase();
    let Some(last) = ref_lower.split('/').next_back() else {
        return;
    };
    let Some(posting) = idx.component_to_paths.get(last) else {
        return;
    };
    let needle = format!("/{ref_lower}/");
    let matched: Vec<&FragmentId> = posting
        .iter()
        .filter_map(|&pi| {
            let (path_lower, rep) = &idx.lower_paths[pi as usize];
            component_aligned(path_lower, &ref_lower, &needle).then_some(rep)
        })
        .collect();
    // A reference resolving to more files than this names a *region*, not a
    // dependency: a bazel label for a directory of hundreds of files says
    // nothing about which of them relates to the change, and emitting an edge
    // to every fragment of every one is the remaining half of the envoy hang
    // (system time: the EdgeDict alone reached page-fault territory).
    if matched.len() > MAX_FILES_PER_PATH_REF {
        return;
    }
    for rep in matched {
        if rep != src_id {
            add_edge(edges, src_id, rep, weight, reverse_factor);
        }
    }
}

/// Same ambiguity bar as `CFamilySemanticWeights::max_files_per_name`, applied
/// to path references.
const MAX_FILES_PER_PATH_REF: usize = 8;

fn component_aligned(path: &str, reference: &str, interior_needle: &str) -> bool {
    if path == reference {
        return true;
    }
    if let Some(rest) = path.strip_suffix(reference) {
        if rest.ends_with('/') {
            return true;
        }
    }
    if let Some(rest) = path.strip_prefix(reference) {
        if rest.starts_with('/') {
            return true;
        }
    }
    // A directory reference matching an interior span of the path
    // (`roles/<name>/tasks/main.yml`) still counts, but only whole components:
    // `config` must not match `preconfigured/`.
    path.contains(interior_needle)
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
        if ref_lower.len() >= SEMANTIC_DISCOVERY.min_ref_length_for_path_match {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FragmentKind;

    fn frag(path: &str) -> Fragment {
        Fragment {
            id: FragmentId::new(std::sync::Arc::from(path), 1, 10),
            kind: FragmentKind::Function,
            content: std::sync::Arc::from("fn x() {}"),
            identifiers: FxHashSet::default(),
            token_count: 10,
            symbol_name: None,
        }
    }

    fn linked_paths(reference: &str, paths: &[&str]) -> Vec<String> {
        let frags: Vec<Fragment> = paths.iter().map(|p| frag(p)).collect();
        let idx = FragmentIndex::new(&frags, None);
        let src = frag("src/origin.yml");
        let mut edges: EdgeDict = FxHashMap::default();
        link_by_path_match(&src.id, reference, &idx, &mut edges, 0.5, 0.5);
        // add_edge also writes the reverse edge, whose dst is the source
        // itself — the question here is only which files got linked.
        let mut out: Vec<String> = edges
            .keys()
            .map(|(_, dst)| dst.path.to_string())
            .filter(|p| p != "src/origin.yml")
            .collect();
        out.sort();
        out.dedup();
        out
    }

    #[test]
    fn a_reference_matches_only_whole_path_components() {
        let paths = [
            "roles/config/tasks/main.yml",
            "src/preconfigured/app.rs",
            "src/config.rs",
            "deep/nested/config",
        ];
        let hit = linked_paths("config", &paths);
        assert!(
            hit.contains(&"roles/config/tasks/main.yml".to_string()),
            "interior whole component must match"
        );
        assert!(
            hit.contains(&"deep/nested/config".to_string()),
            "suffix component must match"
        );
        assert!(
            !hit.contains(&"src/preconfigured/app.rs".to_string()),
            "substring inside a component must NOT match — that fan-out is the envoy hang"
        );
        assert!(
            !hit.contains(&"src/config.rs".to_string()),
            "`config` does not name `config.rs`; a file reference carries its extension"
        );
    }

    #[test]
    fn a_multi_component_reference_still_matches_its_file() {
        let paths = ["source/common/buffer/buffer_impl.h", "other/buffer_impl.h"];
        let hit = linked_paths("common/buffer/buffer_impl.h", &paths);
        assert_eq!(hit, vec!["source/common/buffer/buffer_impl.h".to_string()]);
    }

    #[test]
    fn a_package_prefix_reference_matches_files_under_it() {
        let paths = ["pkg/api/server.go", "pkg/api2/other.go"];
        let hit = linked_paths("pkg/api", &paths);
        assert_eq!(hit, vec!["pkg/api/server.go".to_string()]);
    }
}
