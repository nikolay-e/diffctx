use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, add_edges_from_ids, discover_files_by_refs};

fn is_perl_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    ext == ".pl" || ext == ".pm"
}

static USE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*use\s+([\w:]+)").unwrap());
static REQUIRE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r##"(?m)^\s*require\s+(?:['"]([^'"]+)['"]|([\w:]+))"##).unwrap());
static PACKAGE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*package\s+([\w:]+)").unwrap());
static SUB_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*sub\s+(\w+)").unwrap());
static ISA_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r##"(?m)(?:use\s+(?:parent|base)\s+.*?['"]([\w:]+)['"]|@ISA\s*=.*?['"]([\w:]+)['"])"##,
    )
    .unwrap()
});
static METHOD_CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b(\w+)->(\w+)").unwrap());

fn extract_uses(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();
    for cap in USE_RE.captures_iter(content) {
        let name = &cap[1];
        if !PERL_PRAGMAS.contains(name) {
            refs.insert(name.to_string());
        }
    }
    for cap in REQUIRE_RE.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            refs.insert(m.as_str().to_string());
        }
        if let Some(m) = cap.get(2) {
            refs.insert(m.as_str().to_string());
        }
    }
    refs
}

static PERL_PRAGMAS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [
        "strict",
        "warnings",
        "utf8",
        "lib",
        "constant",
        "vars",
        "feature",
        "Exporter",
        "Carp",
        "Data::Dumper",
        "File::Basename",
    ]
    .iter()
    .copied()
    .collect()
});

fn extract_packages(content: &str) -> FxHashSet<String> {
    PACKAGE_RE
        .captures_iter(content)
        .map(|c| {
            let full = &c[1];
            full.split("::").last().unwrap_or(full).to_string()
        })
        .collect()
}

fn extract_subs(content: &str) -> FxHashSet<String> {
    SUB_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_parents(content: &str) -> FxHashSet<String> {
    let mut parents = FxHashSet::default();
    for cap in ISA_RE.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            parents.insert(
                m.as_str()
                    .split("::")
                    .last()
                    .unwrap_or(m.as_str())
                    .to_string(),
            );
        }
        if let Some(m) = cap.get(2) {
            parents.insert(
                m.as_str()
                    .split("::")
                    .last()
                    .unwrap_or(m.as_str())
                    .to_string(),
            );
        }
    }
    parents
}

pub struct PerlEdgeBuilder;

impl EdgeBuilder for PerlEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_perl_file(Path::new(f.path())))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let use_w = EDGE_WEIGHTS["perl_use"].forward;
        let fn_w = EDGE_WEIGHTS["perl_fn"].forward;
        let method_w = EDGE_WEIGHTS["perl_method"].forward;
        let inherit_w = EDGE_WEIGHTS["perl_inheritance"].forward;
        let reverse_factor = EDGE_WEIGHTS["perl_use"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let mut name_to_defs: FxHashMap<String, Vec<_>> = FxHashMap::default();
        for f in &frags {
            for name in extract_packages(&f.content) {
                name_to_defs
                    .entry(name.to_lowercase())
                    .or_default()
                    .push(f.id.clone());
            }
            for name in extract_subs(&f.content) {
                name_to_defs
                    .entry(name.to_lowercase())
                    .or_default()
                    .push(f.id.clone());
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            let self_defs = extract_subs(&f.content);
            for use_name in extract_uses(&f.content) {
                let leaf = use_name.split("::").last().unwrap_or(&use_name);
                base::link_by_name(&f.id, leaf, &idx, &mut edges, use_w, reverse_factor);
            }
            for parent in extract_parents(&f.content) {
                if let Some(targets) = name_to_defs.get(&parent.to_lowercase()) {
                    add_edges_from_ids(&mut edges, &f.id, targets, inherit_w, reverse_factor);
                }
            }
            for cap in METHOD_CALL_RE.captures_iter(&f.content) {
                let method = &cap[2];
                if self_defs.contains(method) {
                    continue;
                }
                if let Some(targets) = name_to_defs.get(&method.to_lowercase()) {
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
        let pl_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_perl_file(f)).collect();
        if pl_changed.is_empty() {
            return vec![];
        }
        let mut refs = FxHashSet::default();
        for f in &pl_changed {
            if let Some(content) = base::read_file_cached(f, file_cache) {
                refs.extend(extract_uses(&content));
            }
        }
        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
