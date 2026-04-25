use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, FragmentIndex, add_edge, link_by_name};

static WORKSPACE_MEMBERS_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?s)\[workspace\][^\[]*?members\s*=\s*\[(.*?)\]").unwrap());
static PATH_DEP_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r##"(?m)^\s*(\w[\w-]{0,100})\s*=\s*\{[^}]*?path\s*=\s*["']([^"']{1,300})["']"##)
        .unwrap()
});
static FEATURES_SECTION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?ms)^\[features\]\s*\n(.+?)(?:\n\[|\z)").unwrap());
static FEATURE_DEP_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r##"["'](\w[\w-]{0,100})(?:/[^"']*)?["']"##).unwrap());
static STRING_ITEM_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r##"["']([^"']{1,300})["']"##).unwrap());
static BIN_SECTION_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r##"(?s)\[\[bin\]\][^\[]*?path\s*=\s*["']([^"']{1,300})["']"##).unwrap()
});
static LIB_SECTION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r##"(?s)\[lib\][^\[]*?path\s*=\s*["']([^"']{1,300})["']"##).unwrap());

fn is_cargo_toml(path: &Path) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy().to_lowercase() == "cargo.toml")
        .unwrap_or(false)
}

fn is_rust_source(path: &Path) -> bool {
    base::file_ext(path) == ".rs"
}

fn extract_workspace_members(content: &str) -> Vec<String> {
    WORKSPACE_MEMBERS_RE
        .captures(content)
        .map(|c| {
            STRING_ITEM_RE
                .captures_iter(&c[1])
                .map(|m| m[1].to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn extract_path_deps(content: &str) -> Vec<(String, String)> {
    PATH_DEP_RE
        .captures_iter(content)
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .collect()
}

fn extract_feature_deps(content: &str) -> FxHashSet<String> {
    FEATURES_SECTION_RE
        .captures(content)
        .map(|c| {
            FEATURE_DEP_RE
                .captures_iter(&c[1])
                .map(|m| m[1].to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn extract_entry_points(content: &str) -> Vec<String> {
    let mut entries: Vec<String> = BIN_SECTION_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect();
    if let Some(c) = LIB_SECTION_RE.captures(content) {
        entries.push(c[1].to_string());
    }
    for default in ["src/lib.rs", "src/main.rs"] {
        if !entries.iter().any(|e| e == default) {
            entries.push(default.to_string());
        }
    }
    entries
}

pub struct CargoEdgeBuilder;

impl EdgeBuilder for CargoEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let cargo_frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_cargo_toml(Path::new(f.path())))
            .collect();
        if cargo_frags.is_empty() {
            return FxHashMap::default();
        }

        let ws_w = EDGE_WEIGHTS["cargo_workspace"].forward;
        let dep_w = EDGE_WEIGHTS["cargo_path_dep"].forward;
        let entry_w = EDGE_WEIGHTS["cargo_entry_point"].forward;
        let rev = EDGE_WEIGHTS["cargo_workspace"].reverse_factor;

        let idx = FragmentIndex::new(fragments, repo_root);
        let mut edges: EdgeDict = FxHashMap::default();

        let mut cargo_by_dir: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        for f in &cargo_frags {
            let dir = Path::new(f.path())
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            cargo_by_dir.entry(dir).or_default().push(f.id.clone());
        }

        let mut rs_by_path: FxHashMap<String, FragmentId> = FxHashMap::default();
        for f in fragments {
            if is_rust_source(Path::new(f.path())) {
                rs_by_path.insert(f.path().to_string(), f.id.clone());
            }
        }

        for cf in &cargo_frags {
            let parent = Path::new(cf.path()).parent().unwrap_or(Path::new(""));

            for entry in extract_entry_points(&cf.content) {
                let entry_path = parent.join(&entry).to_string_lossy().to_string();
                if let Some(fid) = rs_by_path.get(&entry_path) {
                    if fid != &cf.id {
                        add_edge(&mut edges, &cf.id, fid, entry_w, rev);
                    }
                }
            }

            for (_, rel_path) in extract_path_deps(&cf.content) {
                let dep_dir = parent.join(&rel_path).to_string_lossy().to_string();
                if let Some(fids) = cargo_by_dir.get(&dep_dir) {
                    for fid in fids {
                        if fid != &cf.id {
                            add_edge(&mut edges, &cf.id, fid, dep_w, rev);
                        }
                    }
                }
            }

            for member in extract_workspace_members(&cf.content) {
                let member_dir = parent.join(&member).to_string_lossy().to_string();
                if let Some(fids) = cargo_by_dir.get(&member_dir) {
                    for fid in fids {
                        if fid != &cf.id {
                            add_edge(&mut edges, &cf.id, fid, ws_w, rev);
                        }
                    }
                }
                link_by_name(
                    &cf.id,
                    &format!("{}/Cargo.toml", member),
                    &idx,
                    &mut edges,
                    ws_w,
                    rev,
                );
            }

            let feature_deps = extract_feature_deps(&cf.content);
            let path_deps = extract_path_deps(&cf.content);
            for dep_name in &feature_deps {
                for (name, rel_path) in &path_deps {
                    if name != dep_name {
                        continue;
                    }
                    let dep_dir = parent.join(rel_path).to_string_lossy().to_string();
                    if let Some(fids) = cargo_by_dir.get(&dep_dir) {
                        for fid in fids {
                            if fid != &cf.id {
                                add_edge(&mut edges, &cf.id, fid, dep_w, rev);
                            }
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
        let cargo_changed: Vec<&PathBuf> = changed.iter().filter(|p| is_cargo_toml(p)).collect();
        if cargo_changed.is_empty() {
            return vec![];
        }

        let mut refs = FxHashSet::default();

        for cf in &cargo_changed {
            let content = match base::read_file_cached(cf, file_cache) {
                Some(c) => c,
                None => continue,
            };
            let parent = cf.parent().unwrap_or(Path::new(""));

            for entry in extract_entry_points(&content) {
                refs.insert(parent.join(&entry).to_string_lossy().to_string());
            }
            for (_, rel_path) in extract_path_deps(&content) {
                refs.insert(
                    parent
                        .join(&rel_path)
                        .join("Cargo.toml")
                        .to_string_lossy()
                        .to_string(),
                );
            }
            for member in extract_workspace_members(&content) {
                refs.insert(
                    parent
                        .join(&member)
                        .join("Cargo.toml")
                        .to_string_lossy()
                        .to_string(),
                );
            }
        }

        base::discover_files_by_refs(&refs, changed, candidates, repo_root)
    }

    fn category_label(&self) -> Option<&str> {
        Some("semantic")
    }
}
