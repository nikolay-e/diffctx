use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::{Fragment, FragmentKind};

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edges_from_ids};

fn is_swift_file(path: &Path) -> bool {
    base::has_ext(path, &[".swift"])
}

static IMPORT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*import\s+(?:class|struct|enum|protocol|func\s+)?(\w+)").unwrap()
});
static TYPE_DEF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:(?:public|open|internal|private|fileprivate|final|indirect)\s+)*(?:class|struct|protocol|enum|actor)\s+([A-Z]\w*)").unwrap()
});
static FUNC_DEF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:@\w+(?:\([^)]*\))?\s+)*(?:(?:public|open|internal|private|fileprivate|static|class|final|override|mutating|nonisolated)\s+)*func\s+([a-zA-Z_]\w*)").unwrap()
});
static CONFORMANCE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?:class|struct|enum|actor)\s+\w+\s*:\s*([\w\s,]+)").unwrap());
static EXTENSION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*extension\s+([A-Z]\w*)").unwrap());
static TYPE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w*)\b").unwrap());
static CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([a-zA-Z_]\w*)\s*\(").unwrap());
static SWIFT_KEYWORDS: Lazy<FxHashSet<&'static str>> = Lazy::new(|| {
    base::kw(
        "if else for while switch case return guard where catch throw try await init deinit \
         subscript func class struct enum protocol extension import let var typealias \
         associatedtype in is as super self Self nil true false print assert precondition \
         fatalError",
    )
});

fn extract_imports(content: &str) -> FxHashSet<String> {
    base::captures1(&IMPORT_RE, content).collect()
}

fn extract_type_defs(content: &str) -> FxHashSet<String> {
    let mut defs: FxHashSet<String> = base::captures1(&TYPE_DEF_RE, content).collect();
    defs.extend(base::captures1(&FUNC_DEF_RE, content));
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
    base::captures1(&EXTENSION_RE, content).collect()
}

pub struct SwiftEdgeBuilder;

impl EdgeBuilder for SwiftEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let Some(frags) = base::frags_where(fragments, |f| is_swift_file(Path::new(f.path())))
        else {
            return FxHashMap::default();
        };

        let import_w = EDGE_WEIGHTS["swift_import"].forward;
        let conform_w = EDGE_WEIGHTS["swift_conformance"].forward;
        let ext_w = EDGE_WEIGHTS["swift_extension"].forward;
        let type_w = EDGE_WEIGHTS["swift_type"].forward;
        let reverse_factor = EDGE_WEIGHTS["swift_import"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let mut name_to_defs: FxHashMap<String, Vec<_>> = FxHashMap::default();
        let mut file_type_defs: FxHashMap<&str, FxHashSet<String>> = FxHashMap::default();
        let mut file_type_refs: FxHashMap<&str, FxHashSet<String>> = FxHashMap::default();
        for f in &frags {
            file_type_defs
                .entry(f.path())
                .or_default()
                .extend(base::captures1(&TYPE_DEF_RE, &f.content));
            file_type_defs
                .entry(f.path())
                .or_default()
                .extend(extract_extensions(&f.content));
            file_type_refs
                .entry(f.path())
                .or_default()
                .extend(base::captures1(&TYPE_REF_RE, &f.content));
            let wraps_funcs = f.kind != FragmentKind::Function
                && frags.iter().any(|c| {
                    c.path() == f.path()
                        && c.id != f.id
                        && c.kind == FragmentKind::Function
                        && c.start_line() >= f.start_line()
                        && c.end_line() <= f.end_line()
                });
            let defs: FxHashSet<String> = if wraps_funcs {
                base::captures1(&TYPE_DEF_RE, &f.content).collect()
            } else {
                extract_type_defs(&f.content)
            };
            base::index_lower(&mut name_to_defs, defs, &f.id);
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
                    add_edges_from_ids(&mut edges, &f.id, targets, type_w, reverse_factor);
                }
            }
            for cap in CALL_RE.captures_iter(&f.content) {
                let name = &cap[1];
                if SWIFT_KEYWORDS.contains(name) || self_defs.contains(name) {
                    continue;
                }
                if let Some(targets) = name_to_defs.get(&name.to_lowercase()) {
                    let caller_refs = file_type_refs.get(f.path());
                    let typed: Vec<_> = targets
                        .iter()
                        .filter(|t| {
                            t.path == f.id.path
                                || file_type_defs.get(t.path.as_ref()).is_some_and(|defs| {
                                    caller_refs.is_some_and(|refs| !defs.is_disjoint(refs))
                                })
                        })
                        .cloned()
                        .collect();
                    let chosen = if typed.is_empty() { targets } else { &typed };
                    add_edges_from_ids(&mut edges, &f.id, chosen, type_w, reverse_factor);
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
        base::discover_by_extracted_refs(
            changed,
            candidates,
            repo_root,
            file_cache,
            is_swift_file,
            extract_imports,
        )
    }
}
