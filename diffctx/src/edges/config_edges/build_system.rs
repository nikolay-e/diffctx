use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, FragmentIndex, link_by_name};

const FILE_REF_WEIGHT: f64 = 0.60;
const REVERSE_FACTOR: f64 = 0.35;

static MAKE_INCLUDE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^(?:-)?include\s+(.+)$").unwrap());
static MAKE_RECIPE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\t(.+)$").unwrap());

static CMAKE_INCLUDE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"include\s*\(\s*([^)]+)\)").unwrap());
static CMAKE_ADD_SUBDIR_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"add_subdirectory\s*\(\s*([^\)\s]+)").unwrap());

static SOURCE_FILE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b([a-zA-Z_]\w*\.(?:c|cpp|cc|cxx|h|hpp|hxx|py|sh|go|rs|java))\b").unwrap()
});

fn is_makefile(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name == "makefile"
        || name == "gnumakefile"
        || name.ends_with(".mk")
        || name.ends_with(".mak")
        || name.ends_with(".make")
}

fn is_cmake(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    name == "CMakeLists.txt" || name.to_lowercase().ends_with(".cmake")
}

fn is_build_file(path: &Path) -> bool {
    is_makefile(path) || is_cmake(path)
}

fn extract_makefile_refs(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();

    for cap in MAKE_INCLUDE_RE.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            for token in m.as_str().split_whitespace() {
                let cleaned = token.trim();
                if !cleaned.is_empty() && !cleaned.starts_with('$') {
                    refs.insert(cleaned.to_string());
                }
            }
        }
    }

    for cap in MAKE_RECIPE_RE.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            for file_cap in SOURCE_FILE_RE.captures_iter(m.as_str()) {
                if let Some(fm) = file_cap.get(1) {
                    refs.insert(fm.as_str().to_string());
                }
            }
        }
    }

    refs
}

fn extract_cmake_refs(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();

    for cap in CMAKE_INCLUDE_RE.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            let val = m.as_str().trim();
            if !val.is_empty() && !val.starts_with('$') {
                refs.insert(val.to_string());
            }
        }
    }

    for cap in CMAKE_ADD_SUBDIR_RE.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            let val = m.as_str().trim();
            if !val.is_empty() && !val.starts_with('$') {
                refs.insert(val.to_string());
            }
        }
    }

    for file_cap in SOURCE_FILE_RE.captures_iter(content) {
        if let Some(m) = file_cap.get(1) {
            refs.insert(m.as_str().to_string());
        }
    }

    refs
}

fn extract_refs(path: &Path, content: &str) -> FxHashSet<String> {
    if is_cmake(path) {
        extract_cmake_refs(content)
    } else {
        extract_makefile_refs(content)
    }
}

pub struct BuildSystemEdgeBuilder;

impl EdgeBuilder for BuildSystemEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let build_frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_build_file(Path::new(f.path())))
            .collect();
        if build_frags.is_empty() {
            return EdgeDict::default();
        }

        let idx = FragmentIndex::new(fragments, repo_root);
        let mut edges = EdgeDict::default();

        for bf in &build_frags {
            let refs = extract_refs(Path::new(bf.path()), &bf.content);
            for r in &refs {
                link_by_name(&bf.id, r, &idx, &mut edges, FILE_REF_WEIGHT, REVERSE_FACTOR);
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
        let build_files: Vec<&PathBuf> = changed.iter().filter(|p| is_build_file(p)).collect();
        if build_files.is_empty() {
            return vec![];
        }

        let mut refs = FxHashSet::default();

        for bf in &build_files {
            let content = match base::read_file_cached(bf, file_cache) {
                Some(c) => c,
                None => continue,
            };
            refs.extend(extract_refs(bf, &content));
        }

        base::discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
