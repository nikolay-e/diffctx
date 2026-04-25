use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::extensions::SWIFT_EXTENSIONS;
use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, add_edges_from_ids, discover_files_by_refs};

fn is_swift_file(path: &Path) -> bool {
    base::file_ext(path) == ".swift"
}

static IMPORT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*import\s+(?:class|struct|enum|protocol|func\s+)?(\w+)").unwrap()
});
static TYPE_DEF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:public|open|internal|private|fileprivate)?\s*(?:final\s+)?(?:class|struct|protocol|enum|actor)\s+([A-Z]\w*)").unwrap()
});
static FUNC_DEF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:public|open|internal|private|fileprivate|static|class)?\s*func\s+([a-zA-Z_]\w*)").unwrap()
});
static CONFORMANCE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?:class|struct|enum|actor)\s+\w+\s*:\s*([\w\s,]+)").unwrap());
static EXTENSION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*extension\s+([A-Z]\w*)").unwrap());
static TYPE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w*)\b").unwrap());

fn extract_imports(content: &str) -> FxHashSet<String> {
    IMPORT_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_type_defs(content: &str) -> FxHashSet<String> {
    let mut defs: FxHashSet<String> = TYPE_DEF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect();
    defs.extend(FUNC_DEF_RE.captures_iter(content).map(|c| c[1].to_string()));
    defs
}

fn extract_conformances(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();
    for cap in CONFORMANCE_RE.captures_iter(content) {
        for part in cap[1].split(',') {
            let name = part.trim();
            if !name.is_empty() && name.starts_with(|c: char| c.is_uppercase()) {
                refs.insert(name.to_string());
            }
        }
    }
    refs
}

fn extract_extensions(content: &str) -> FxHashSet<String> {
    EXTENSION_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

pub struct SwiftEdgeBuilder;

impl EdgeBuilder for SwiftEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_swift_file(Path::new(f.path())))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let import_w = EDGE_WEIGHTS["swift_import"].forward;
        let conform_w = EDGE_WEIGHTS["swift_conformance"].forward;
        let ext_w = EDGE_WEIGHTS["swift_extension"].forward;
        let type_w = EDGE_WEIGHTS["swift_type"].forward;
        let reverse_factor = EDGE_WEIGHTS["swift_import"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let mut name_to_defs: FxHashMap<String, Vec<_>> = FxHashMap::default();
        for f in &frags {
            for name in extract_type_defs(&f.content) {
                name_to_defs
                    .entry(name.to_lowercase())
                    .or_default()
                    .push(f.id.clone());
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            let self_defs = extract_type_defs(&f.content);
            for imp in extract_imports(&f.content) {
                base::link_by_name(&f.id, &imp, &idx, &mut edges, import_w, reverse_factor);
            }
            for conf in extract_conformances(&f.content) {
                if let Some(targets) = name_to_defs.get(&conf.to_lowercase()) {
                    add_edges_from_ids(&mut edges, &f.id, targets, conform_w, reverse_factor);
                }
            }
            for ext_name in extract_extensions(&f.content) {
                if let Some(targets) = name_to_defs.get(&ext_name.to_lowercase()) {
                    add_edges_from_ids(&mut edges, &f.id, targets, ext_w, reverse_factor);
                }
            }
            for cap in TYPE_REF_RE.captures_iter(&f.content) {
                let name = &cap[1];
                if self_defs.contains(name) {
                    continue;
                }
                if let Some(targets) = name_to_defs.get(&name.to_lowercase()) {
                    for t in targets {
                        if t != &f.id {
                            add_edge(&mut edges, &f.id, t, type_w, reverse_factor);
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
        let sw_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_swift_file(f)).collect();
        if sw_changed.is_empty() {
            return vec![];
        }
        let mut refs = FxHashSet::default();
        for f in &sw_changed {
            if let Some(content) = base::read_file_cached(f, file_cache) {
                refs.extend(extract_imports(&content));
            }
        }
        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
