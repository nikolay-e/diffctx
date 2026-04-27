use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::edge_weights::PYTHON_SEMANTIC;
use crate::config::extensions::PYTHON_EXTENSIONS;
use crate::config::weights::LANG_WEIGHTS;
use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, path_to_module};

fn is_python_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    PYTHON_EXTENSIONS.contains(ext.as_str())
}

static IMPORT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*import\s+([\w.]+)").unwrap());
static FROM_IMPORT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*from\s+([\w.]+)\s+import\s+(.+)").unwrap());
static CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Za-z_]\w*)\s*\(").unwrap());
static TYPE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w*)\b").unwrap());
static DEF_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*(?:def|class|async\s+def)\s+([A-Za-z_]\w*)").unwrap());

fn extract_imports(content: &str, path: &Path, repo_root: Option<&Path>) -> FxHashSet<String> {
    let mut imports = FxHashSet::default();
    for cap in IMPORT_RE.captures_iter(content) {
        imports.insert(cap[1].to_string());
    }
    for cap in FROM_IMPORT_RE.captures_iter(content) {
        let module = &cap[1];
        if module.starts_with('.') {
            if let Some(parent) = path.parent() {
                let module_path = path_to_module(parent, repo_root);
                if !module_path.is_empty() {
                    imports.insert(module_path);
                }
            }
        } else {
            imports.insert(module.to_string());
            let parts: Vec<&str> = module.split('.').collect();
            for i in 1..parts.len() {
                imports.insert(parts[..i].join("."));
            }
        }
    }
    imports
}

fn extract_defines(content: &str) -> FxHashSet<String> {
    DEF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_calls(content: &str) -> FxHashSet<String> {
    CALL_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .filter(|n| !PY_KEYWORDS.contains(n.as_str()))
        .collect()
}

fn extract_type_refs(content: &str) -> FxHashSet<String> {
    TYPE_REF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

static PY_KEYWORDS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [
        "if",
        "for",
        "while",
        "return",
        "def",
        "class",
        "import",
        "from",
        "as",
        "with",
        "try",
        "except",
        "finally",
        "raise",
        "pass",
        "break",
        "continue",
        "yield",
        "lambda",
        "assert",
        "del",
        "elif",
        "else",
        "global",
        "nonlocal",
        "and",
        "or",
        "not",
        "is",
        "in",
        "async",
        "await",
        "True",
        "False",
        "None",
        "print",
        "len",
        "range",
        "type",
        "list",
        "dict",
        "set",
        "tuple",
        "str",
        "int",
        "float",
        "bool",
        "super",
        "isinstance",
        "hasattr",
        "getattr",
        "setattr",
        "property",
        "staticmethod",
        "classmethod",
    ]
    .iter()
    .copied()
    .collect()
});

pub struct PythonEdgeBuilder;

impl EdgeBuilder for PythonEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let py_frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_python_file(Path::new(f.path())))
            .collect();
        if py_frags.is_empty() {
            return FxHashMap::default();
        }

        let weights = LANG_WEIGHTS.get("python").expect("python weights");
        let call_weight = weights.call;
        let symbol_ref_weight = weights.symbol_ref;
        let type_ref_weight = weights.type_ref;

        let mut name_to_defs: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut frag_defines: FxHashMap<FragmentId, FxHashSet<String>> = FxHashMap::default();
        let mut module_to_frags: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();

        for f in &py_frags {
            let defines = extract_defines(&f.content);
            for name in &defines {
                name_to_defs
                    .entry(name.clone())
                    .or_default()
                    .push(f.id.clone());
            }
            frag_defines.insert(f.id.clone(), defines);

            let module = path_to_module(Path::new(f.path()), repo_root);
            if !module.is_empty() {
                module_to_frags
                    .entry(module)
                    .or_default()
                    .push(f.id.clone());
            }
        }

        let frag_imports: FxHashMap<FragmentId, FxHashSet<String>> = py_frags
            .iter()
            .map(|f| {
                let imports = extract_imports(&f.content, Path::new(f.path()), repo_root);
                (f.id.clone(), imports)
            })
            .collect();

        let frag_to_module: FxHashMap<FragmentId, String> = py_frags
            .iter()
            .filter_map(|f| {
                let m = path_to_module(Path::new(f.path()), repo_root);
                if m.is_empty() {
                    None
                } else {
                    Some((f.id.clone(), m))
                }
            })
            .collect();

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &py_frags {
            let self_defs = frag_defines.get(&f.id).cloned().unwrap_or_default();
            let src_imports = frag_imports.get(&f.id).cloned().unwrap_or_default();

            let calls = extract_calls(&f.content);
            let type_refs = extract_type_refs(&f.content);
            let refs: FxHashSet<String> = f
                .identifiers
                .iter()
                .filter(|id| !self_defs.contains(*id))
                .cloned()
                .collect();

            for (ref_set, base_weight) in [
                (&calls, call_weight),
                (&refs, symbol_ref_weight),
                (&type_refs, type_ref_weight),
            ] {
                for name in ref_set {
                    if self_defs.contains(name) {
                        continue;
                    }
                    if let Some(dst_ids) = name_to_defs.get(name) {
                        for dst_id in dst_ids {
                            if dst_id == &f.id {
                                continue;
                            }
                            let dst_module =
                                frag_to_module.get(dst_id).map(|s| s.as_str()).unwrap_or("");
                            let confirmed =
                                !dst_module.is_empty() && src_imports.contains(dst_module);
                            let factor = if confirmed {
                                PYTHON_SEMANTIC.import_confirmed_boost
                            } else {
                                PYTHON_SEMANTIC.import_unconfirmed_penalty
                            };
                            let w = base_weight * factor;
                            let key_fwd = (f.id.clone(), dst_id.clone());
                            let existing = edges.get(&key_fwd).copied().unwrap_or(0.0);
                            if w > existing {
                                edges.insert(key_fwd, w);
                            }
                            let rev_w = w * PYTHON_SEMANTIC.reverse_factor;
                            let key_rev = (dst_id.clone(), f.id.clone());
                            let existing_rev = edges.get(&key_rev).copied().unwrap_or(0.0);
                            if rev_w > existing_rev {
                                edges.insert(key_rev, rev_w);
                            }
                        }
                    }
                }
            }

            for imp in &src_imports {
                if let Some(targets) = module_to_frags.get(imp) {
                    for tgt in targets {
                        if tgt == &f.id {
                            continue;
                        }
                        let key = (f.id.clone(), tgt.clone());
                        let existing = edges.get(&key).copied().unwrap_or(0.0);
                        if PYTHON_SEMANTIC.import_weight > existing {
                            edges.insert(key, PYTHON_SEMANTIC.import_weight);
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
        let py_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_python_file(f)).collect();
        if py_changed.is_empty() {
            return vec![];
        }

        let mut file_to_module: FxHashMap<PathBuf, String> = FxHashMap::default();
        let mut module_to_files: FxHashMap<String, Vec<PathBuf>> = FxHashMap::default();
        let mut file_to_imports: FxHashMap<PathBuf, FxHashSet<String>> = FxHashMap::default();

        for f in candidates {
            if !is_python_file(f) {
                continue;
            }
            let module = path_to_module(f, repo_root);
            if !module.is_empty() {
                file_to_module.insert(f.clone(), module.clone());
                module_to_files
                    .entry(module.clone())
                    .or_default()
                    .push(f.clone());
                let parts: Vec<&str> = module.split('.').collect();
                for i in 1..parts.len() {
                    module_to_files
                        .entry(parts[..i].join("."))
                        .or_default()
                        .push(f.clone());
                }
            }
            let content = base::read_file_cached(f, file_cache);
            if let Some(c) = content {
                file_to_imports.insert(f.clone(), extract_imports(&c, f, repo_root));
            }
        }

        let changed_set: FxHashSet<PathBuf> = changed.iter().cloned().collect();
        let mut discovered: FxHashSet<PathBuf> = FxHashSet::default();
        let mut frontier: FxHashSet<PathBuf> = py_changed.iter().map(|f| (*f).clone()).collect();

        for _ in 0..2 {
            let mut next_frontier: FxHashSet<PathBuf> = FxHashSet::default();
            for f in &frontier {
                let f_imports = file_to_imports.get(f).cloned().unwrap_or_default();
                for imp in &f_imports {
                    if let Some(targets) = module_to_files.get(imp) {
                        for target in targets {
                            if !changed_set.contains(target) && !discovered.contains(target) {
                                discovered.insert(target.clone());
                                next_frontier.insert(target.clone());
                            }
                        }
                    }
                }
                let f_module = file_to_module
                    .get(f)
                    .cloned()
                    .unwrap_or_else(|| path_to_module(f, repo_root));
                if !f_module.is_empty() {
                    for (candidate, cand_imports) in &file_to_imports {
                        if !changed_set.contains(candidate)
                            && !discovered.contains(candidate)
                            && cand_imports.contains(&f_module)
                        {
                            discovered.insert(candidate.clone());
                            next_frontier.insert(candidate.clone());
                        }
                    }
                }
            }
            if next_frontier.is_empty() {
                break;
            }
            frontier = next_frontier;
        }

        let mut result: Vec<PathBuf> = discovered.into_iter().collect();
        result.sort();
        result
    }
}
