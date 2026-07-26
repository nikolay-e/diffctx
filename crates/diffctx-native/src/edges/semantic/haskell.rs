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

static HASKELL_EXTENSIONS: Lazy<FxHashSet<&str>> =
    Lazy::new(|| [".hs", ".lhs"].iter().copied().collect());

fn is_haskell_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    HASKELL_EXTENSIONS.contains(ext.as_str())
}

static IMPORT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*import\s+(?:qualified\s+)?([A-Z][\w.]+)").unwrap());
static MODULE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*module\s+([A-Z][\w.]+)").unwrap());
static DATA_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*(?:data|newtype|type)\s+([A-Z]\w+)").unwrap());
static CLASS_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*class\s+(?:.*?=>\s*)?([A-Z]\w+)").unwrap());
static INSTANCE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*instance\s+.*?\b([A-Z]\w+)\s+([A-Z]\w+)").unwrap());
static TYPE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w+)\b").unwrap());
static FUNC_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^([a-z_]\w*)\s*::").unwrap());
static CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([a-z_]\w+)\b").unwrap());

static HASKELL_KEYWORDS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [
        "module",
        "where",
        "import",
        "qualified",
        "as",
        "hiding",
        "data",
        "newtype",
        "type",
        "class",
        "instance",
        "deriving",
        "if",
        "then",
        "else",
        "case",
        "of",
        "let",
        "in",
        "do",
        "return",
        "where",
        "forall",
        "foreign",
        "default",
        "infixl",
        "infixr",
        "infix",
        "otherwise",
        "undefined",
        "error",
        "show",
        "read",
        "map",
        "filter",
        "foldl",
        "foldr",
        "head",
        "tail",
        "null",
        "length",
        "print",
        "putStrLn",
        "getLine",
        "main",
        "IO",
        "Maybe",
        "Just",
        "Nothing",
        "Either",
        "Left",
        "Right",
        "True",
        "False",
        "Bool",
        "Int",
        "Integer",
        "Float",
        "Double",
        "Char",
        "String",
    ]
    .iter()
    .copied()
    .collect()
});

fn extract_imports(content: &str) -> FxHashSet<String> {
    IMPORT_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_modules(content: &str) -> FxHashSet<String> {
    MODULE_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_defines(content: &str) -> FxHashSet<String> {
    let mut defs = FxHashSet::default();
    for cap in DATA_RE.captures_iter(content) {
        defs.insert(cap[1].to_string());
    }
    for cap in CLASS_RE.captures_iter(content) {
        defs.insert(cap[1].to_string());
    }
    for cap in FUNC_RE.captures_iter(content) {
        defs.insert(cap[1].to_string());
    }
    defs
}

fn extract_instance_refs(content: &str) -> Vec<(String, String)> {
    INSTANCE_RE
        .captures_iter(content)
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .collect()
}

fn extract_type_refs(content: &str) -> FxHashSet<String> {
    TYPE_REF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .filter(|n| !HASKELL_KEYWORDS.contains(n.as_str()))
        .collect()
}

fn extract_calls(content: &str) -> FxHashSet<String> {
    CALL_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .filter(|n| !HASKELL_KEYWORDS.contains(n.as_str()))
        .collect()
}

pub struct HaskellEdgeBuilder;

impl EdgeBuilder for HaskellEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let hs_frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_haskell_file(Path::new(f.path())))
            .collect();
        if hs_frags.is_empty() {
            return FxHashMap::default();
        }

        let import_weight = EDGE_WEIGHTS["haskell_import"].forward;
        let type_weight = EDGE_WEIGHTS["haskell_type"].forward;
        let fn_weight = EDGE_WEIGHTS["haskell_fn"].forward;
        let instance_weight = EDGE_WEIGHTS["haskell_instance"].forward;
        let reverse_factor = EDGE_WEIGHTS["haskell_import"].reverse_factor;

        let idx = FragmentIndex::new(fragments, repo_root);

        let mut name_to_defs: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut frag_defines: FxHashMap<FragmentId, FxHashSet<String>> = FxHashMap::default();
        let mut module_to_frags: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();

        for f in &hs_frags {
            let defs = extract_defines(&f.content);
            for name in &defs {
                name_to_defs
                    .entry(name.clone())
                    .or_default()
                    .push(f.id.clone());
            }
            frag_defines.insert(f.id.clone(), defs);

            let modules = extract_modules(&f.content);
            for m in &modules {
                module_to_frags
                    .entry(m.clone())
                    .or_default()
                    .push(f.id.clone());
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &hs_frags {
            let self_defs = frag_defines.get(&f.id).cloned().unwrap_or_default();

            let imports = extract_imports(&f.content);
            for imp in &imports {
                if let Some(targets) = module_to_frags.get(imp) {
                    for tgt in targets {
                        if tgt != &f.id {
                            add_edge(&mut edges, &f.id, tgt, import_weight, reverse_factor);
                        }
                    }
                }
                link_by_name(&f.id, imp, &idx, &mut edges, import_weight, reverse_factor);
            }

            let instances = extract_instance_refs(&f.content);
            for (class_name, type_name) in &instances {
                for name in [class_name, type_name] {
                    if let Some(dst_ids) = name_to_defs.get(name) {
                        for dst_id in dst_ids {
                            if dst_id != &f.id {
                                add_edge(
                                    &mut edges,
                                    &f.id,
                                    dst_id,
                                    instance_weight,
                                    reverse_factor,
                                );
                            }
                        }
                    }
                }
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
        let hs_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_haskell_file(f)).collect();
        if hs_changed.is_empty() {
            return vec![];
        }

        let mut all_refs = FxHashSet::default();
        for f in &hs_changed {
            let content = base::read_file_cached(f, file_cache);
            if let Some(c) = content {
                all_refs.extend(extract_imports(&c));
                all_refs.extend(extract_modules(&c));
            }
        }

        discover_files_by_refs(&all_refs, changed, candidates, repo_root)
    }
}
