use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{
    self, EdgeBuilder, FragmentIndex, add_edge, discover_files_by_refs, link_by_name,
};

static DART_EXTENSIONS: Lazy<FxHashSet<&str>> = Lazy::new(|| [".dart"].iter().copied().collect());

fn is_dart_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    DART_EXTENSIONS.contains(ext.as_str())
}

static IMPORT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^\s*import\s+['"]([^'"]+)['"]"#).unwrap());
static EXPORT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^\s*export\s+['"]([^'"]+)['"]"#).unwrap());
static PART_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^\s*part\s+['"]([^'"]+)['"]"#).unwrap());
static PART_OF_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^\s*part\s+of\s+['"]([^'"]+)['"]"#).unwrap());
static CLASS_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*(?:abstract\s+)?class\s+(\w+)").unwrap());
static MIXIN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*mixin\s+(\w+)").unwrap());
static EXTENSION_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*extension\s+(\w+)").unwrap());
static FUNC_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*(?:\w+\s+)*(\w+)\s*[<(]").unwrap());
static TYPE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w+)\b").unwrap());
static CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([a-z_]\w+)\s*\(").unwrap());

static DART_KEYWORDS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [
        "if",
        "else",
        "for",
        "while",
        "do",
        "switch",
        "case",
        "break",
        "continue",
        "return",
        "var",
        "final",
        "const",
        "void",
        "null",
        "true",
        "false",
        "new",
        "this",
        "super",
        "class",
        "extends",
        "implements",
        "with",
        "abstract",
        "import",
        "export",
        "library",
        "part",
        "typedef",
        "enum",
        "mixin",
        "extension",
        "async",
        "await",
        "yield",
        "try",
        "catch",
        "finally",
        "throw",
        "rethrow",
        "assert",
        "in",
        "is",
        "as",
        "dynamic",
        "Function",
        "String",
        "int",
        "double",
        "bool",
        "List",
        "Map",
        "Set",
        "Future",
        "Stream",
        "Iterable",
        "Object",
        "Null",
        "Never",
        "Type",
        "print",
    ]
    .iter()
    .copied()
    .collect()
});

fn extract_refs(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();
    for cap in IMPORT_RE.captures_iter(content) {
        refs.insert(cap[1].to_string());
    }
    for cap in EXPORT_RE.captures_iter(content) {
        refs.insert(cap[1].to_string());
    }
    for cap in PART_RE.captures_iter(content) {
        refs.insert(cap[1].to_string());
    }
    for cap in PART_OF_RE.captures_iter(content) {
        refs.insert(cap[1].to_string());
    }
    refs
}

fn extract_defines(content: &str) -> FxHashSet<String> {
    let mut defs = FxHashSet::default();
    for cap in CLASS_RE.captures_iter(content) {
        defs.insert(cap[1].to_string());
    }
    for cap in MIXIN_RE.captures_iter(content) {
        defs.insert(cap[1].to_string());
    }
    for cap in EXTENSION_RE.captures_iter(content) {
        defs.insert(cap[1].to_string());
    }
    for cap in FUNC_RE.captures_iter(content) {
        let name = &cap[1];
        if !DART_KEYWORDS.contains(name) {
            defs.insert(name.to_string());
        }
    }
    defs
}

fn extract_type_refs(content: &str) -> FxHashSet<String> {
    TYPE_REF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .filter(|n| !DART_KEYWORDS.contains(n.as_str()))
        .collect()
}

fn extract_calls(content: &str) -> FxHashSet<String> {
    CALL_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .filter(|n| !DART_KEYWORDS.contains(n.as_str()))
        .collect()
}

pub struct DartEdgeBuilder;

impl EdgeBuilder for DartEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let dart_frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_dart_file(Path::new(f.path())))
            .collect();
        if dart_frags.is_empty() {
            return FxHashMap::default();
        }

        let import_weight = EDGE_WEIGHTS["dart_import"].forward;
        let type_weight = EDGE_WEIGHTS["dart_type"].forward;
        let fn_weight = EDGE_WEIGHTS["dart_fn"].forward;
        let inheritance_weight = EDGE_WEIGHTS["dart_inheritance"].forward;
        let reverse_factor = EDGE_WEIGHTS["dart_import"].reverse_factor;

        let idx = FragmentIndex::new(fragments, _repo_root);

        let mut name_to_defs: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut frag_defines: FxHashMap<FragmentId, FxHashSet<String>> = FxHashMap::default();

        for f in &dart_frags {
            let defs = extract_defines(&f.content);
            for name in &defs {
                name_to_defs
                    .entry(name.clone())
                    .or_default()
                    .push(f.id.clone());
            }
            frag_defines.insert(f.id.clone(), defs);
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &dart_frags {
            let self_defs = frag_defines.get(&f.id).cloned().unwrap_or_default();

            let file_refs = extract_refs(&f.content);
            for r in &file_refs {
                link_by_name(&f.id, r, &idx, &mut edges, import_weight, reverse_factor);
            }

            let type_refs = extract_type_refs(&f.content);
            for name in &type_refs {
                if self_defs.contains(name) {
                    continue;
                }
                if let Some(dst_ids) = name_to_defs.get(name) {
                    for dst_id in dst_ids {
                        if dst_id != &f.id {
                            add_edge(&mut edges, &f.id, dst_id, type_weight, reverse_factor);
                        }
                    }
                }
            }

            let calls = extract_calls(&f.content);
            for name in &calls {
                if self_defs.contains(name) {
                    continue;
                }
                if let Some(dst_ids) = name_to_defs.get(name) {
                    for dst_id in dst_ids {
                        if dst_id != &f.id {
                            add_edge(&mut edges, &f.id, dst_id, fn_weight, reverse_factor);
                        }
                    }
                }
            }

            for ident in &f.identifiers {
                if self_defs.contains(ident) {
                    continue;
                }
                if let Some(dst_ids) = name_to_defs.get(ident) {
                    for dst_id in dst_ids {
                        if dst_id != &f.id {
                            add_edge(
                                &mut edges,
                                &f.id,
                                dst_id,
                                inheritance_weight,
                                reverse_factor,
                            );
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
        let dart_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_dart_file(f)).collect();
        if dart_changed.is_empty() {
            return vec![];
        }

        let mut all_refs = FxHashSet::default();
        for f in &dart_changed {
            let content = base::read_file_cached(f, file_cache);
            if let Some(c) = content {
                all_refs.extend(extract_refs(&c));
            }
        }

        discover_files_by_refs(&all_refs, changed, candidates, repo_root)
    }
}
