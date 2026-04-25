use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{
    self, EdgeBuilder, FragmentIndex, add_edge, discover_files_by_refs, link_by_path_match,
};

static BAZEL_NAMES: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    ["BUILD", "BUILD.bazel", "WORKSPACE", "WORKSPACE.bazel"]
        .iter()
        .copied()
        .collect()
});

static DEPS_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"["']([/@][^"']{1,300})["']"#).unwrap());
static LOAD_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"load\(\s*["']([^"']{1,300})["']\s*,"#).unwrap());
static SRCS_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"["']([^"']{1,300}\.\w{1,10})["']"#).unwrap());
static LABEL_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"//([^:"']{1,200}):([^"'\s,\]]{1,200})"#).unwrap());

fn is_bazel_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if BAZEL_NAMES.contains(name.as_str()) {
        return true;
    }
    let ext = base::file_ext(path);
    ext == ".bzl" || ext == ".bazel"
}

fn extract_labels(content: &str) -> FxHashSet<String> {
    DEPS_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_loads(content: &str) -> FxHashSet<String> {
    LOAD_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_srcs(content: &str) -> FxHashSet<String> {
    let mut srcs = FxHashSet::default();
    let mut in_srcs = false;
    for line in content.lines() {
        let stripped = line.trim();
        if stripped.contains("srcs") && stripped.contains('=') {
            in_srcs = true;
        }
        if in_srcs {
            for m in SRCS_RE.captures_iter(line) {
                srcs.insert(m[1].to_string());
            }
            if stripped.contains(']') {
                in_srcs = false;
            }
        }
    }
    srcs
}

fn label_to_path(label: &str) -> Option<String> {
    if let Some(c) = LABEL_RE.captures(label) {
        return Some(c[1].to_string());
    }
    if label.starts_with("//") {
        let cleaned = label
            .trim_start_matches('/')
            .split(':')
            .next()
            .unwrap_or("");
        if !cleaned.is_empty() {
            return Some(cleaned.to_string());
        }
    }
    None
}

fn ref_to_filename(r: &str) -> String {
    r.trim_end_matches('/')
        .split('/')
        .next_back()
        .unwrap_or(r)
        .split(':')
        .next_back()
        .unwrap_or(r)
        .to_lowercase()
}

pub struct BazelEdgeBuilder;

impl EdgeBuilder for BazelEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_bazel_file(Path::new(f.path())))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let deps_w = EDGE_WEIGHTS["bazel_deps"].forward;
        let load_w = EDGE_WEIGHTS["bazel_load"].forward;
        let srcs_w = EDGE_WEIGHTS["bazel_srcs"].forward;
        let rev = EDGE_WEIGHTS["bazel_deps"].reverse_factor;

        let idx = FragmentIndex::new(fragments, repo_root);
        let mut edges: EdgeDict = FxHashMap::default();

        for bf in &frags {
            for label in extract_labels(&bf.content) {
                if let Some(path) = label_to_path(&label) {
                    for build_name in ["BUILD", "BUILD.bazel"] {
                        link_by_path_match(
                            &bf.id,
                            &format!("{}/{}", path, build_name),
                            &idx,
                            &mut edges,
                            deps_w,
                            rev,
                        );
                    }
                    link_by_path_match(&bf.id, &path, &idx, &mut edges, deps_w, rev);
                }
            }

            for load in extract_loads(&bf.content) {
                let filename = ref_to_filename(&load);
                let stem = filename.strip_suffix(".bzl").unwrap_or(&filename);
                let mut linked = false;
                for (name, frag_ids) in &idx.by_name {
                    if name == &filename || name == stem {
                        for fid in frag_ids {
                            if fid != &bf.id {
                                add_edge(&mut edges, &bf.id, fid, load_w, rev);
                                linked = true;
                                break;
                            }
                        }
                        if linked {
                            break;
                        }
                    }
                }
                if !linked {
                    if let Some(path) = label_to_path(&load) {
                        link_by_path_match(&bf.id, &path, &idx, &mut edges, load_w, rev);
                    }
                }
            }

            let build_parent = Path::new(bf.path()).parent().unwrap_or(Path::new(""));
            for src in extract_srcs(&bf.content) {
                let src_lower = src.to_lowercase();
                let mut found = false;
                if let Some(frag_ids) = idx.by_name.get(&src_lower) {
                    for fid in frag_ids {
                        if fid == &bf.id {
                            continue;
                        }
                        let frag_parent = Path::new(fid.path.as_ref()).parent();
                        if frag_parent == Some(build_parent) {
                            add_edge(&mut edges, &bf.id, fid, srcs_w, rev);
                            found = true;
                            break;
                        }
                    }
                }
                if !found {
                    let rel = build_parent.join(&src).to_string_lossy().to_string();
                    link_by_path_match(&bf.id, &rel, &idx, &mut edges, srcs_w, rev);
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
        let bazel_changed: Vec<&PathBuf> = changed.iter().filter(|p| is_bazel_file(p)).collect();
        if bazel_changed.is_empty() {
            return vec![];
        }

        let mut refs = FxHashSet::default();
        for f in &bazel_changed {
            let content = match base::read_file_cached(f, file_cache) {
                Some(c) => c,
                None => continue,
            };
            for label in extract_labels(&content) {
                if let Some(path) = label_to_path(&label) {
                    refs.insert(format!("{}/BUILD", path));
                    refs.insert(format!("{}/BUILD.bazel", path));
                    refs.insert(path);
                }
            }
            for load in extract_loads(&content) {
                if let Some(path) = label_to_path(&load) {
                    refs.insert(path);
                }
                refs.insert(ref_to_filename(&load));
            }
            for src in extract_srcs(&content) {
                let parent = f.parent().unwrap_or(Path::new(""));
                refs.insert(parent.join(&src).to_string_lossy().to_string());
                refs.insert(src);
            }
        }

        let changed_names: FxHashSet<String> = bazel_changed
            .iter()
            .filter_map(|f| f.file_name().map(|n| n.to_string_lossy().to_lowercase()))
            .collect();
        let mut changed_paths: FxHashSet<String> = FxHashSet::default();
        for f in &bazel_changed {
            changed_paths.insert(f.to_string_lossy().to_string());
            if base::file_ext(f) == ".bzl" {
                if let Some(stem) = f.file_stem() {
                    changed_paths.insert(stem.to_string_lossy().to_string());
                }
            }
        }

        for candidate in candidates {
            if !is_bazel_file(candidate) {
                continue;
            }
            if let Some(content) = base::read_file_cached(candidate, file_cache) {
                for load in extract_loads(&content) {
                    let load_file = ref_to_filename(&load);
                    if changed_names.contains(&load_file)
                        || changed_paths.iter().any(|cp| load.contains(cp.as_str()))
                    {
                        if let Some(name) = candidate.file_name() {
                            refs.insert(name.to_string_lossy().to_lowercase());
                        }
                    }
                }
            }
        }

        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }

    fn category_label(&self) -> Option<&str> {
        Some("semantic")
    }
}
