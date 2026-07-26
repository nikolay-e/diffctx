use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, discover_files_by_refs};

fn is_nim_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    ext == ".nim" || ext == ".nims"
}

static IMPORT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*import\s+([\w/,\s]+)").unwrap());
static FROM_IMPORT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*from\s+([\w/]+)\s+import\s+(.+)").unwrap());
static INCLUDE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*include\s+([\w/]+)").unwrap());
static PROC_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:proc|func|method|iterator|converter|template|macro)\s+(\w+)").unwrap()
});
static TYPE_SINGLE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s+(\w+)\*?\s*=\s*(?:object|ref|enum|distinct|concept)").unwrap()
});
static CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b(\w+)\s*[\(\[]").unwrap());

static NIM_KEYWORDS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [
        "if",
        "elif",
        "else",
        "when",
        "case",
        "of",
        "for",
        "while",
        "block",
        "break",
        "continue",
        "return",
        "result",
        "proc",
        "func",
        "method",
        "var",
        "let",
        "const",
        "type",
        "import",
        "from",
        "include",
        "export",
        "template",
        "macro",
        "iterator",
        "converter",
        "object",
        "ref",
        "ptr",
        "nil",
        "true",
        "false",
        "and",
        "or",
        "not",
        "xor",
        "div",
        "mod",
        "echo",
        "assert",
        "doAssert",
        "len",
        "add",
        "del",
        "new",
        "newSeq",
    ]
    .iter()
    .copied()
    .collect()
});

fn extract_imports(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();
    for cap in IMPORT_RE.captures_iter(content) {
        for part in cap[1].split(',') {
            let name = part.trim().split('/').last().unwrap_or("").trim();
            if !name.is_empty() {
                refs.insert(name.to_string());
            }
        }
    }
    for cap in FROM_IMPORT_RE.captures_iter(content) {
        refs.insert(cap[1].split('/').last().unwrap_or(&cap[1]).to_string());
    }
    refs.extend(
        INCLUDE_RE
            .captures_iter(content)
            .map(|c| c[1].split('/').last().unwrap_or(&c[1]).to_string()),
    );
    refs
}

fn extract_defs(content: &str) -> FxHashSet<String> {
    let mut defs: FxHashSet<String> = PROC_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect();
    defs.extend(
        TYPE_SINGLE_RE
            .captures_iter(content)
            .map(|c| c[1].to_string()),
    );
    defs
}

pub struct NimEdgeBuilder;

impl EdgeBuilder for NimEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_nim_file(Path::new(f.path())))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let import_w = EDGE_WEIGHTS["nim_import"].forward;
        let type_w = EDGE_WEIGHTS["nim_type"].forward;
        let fn_w = EDGE_WEIGHTS["nim_fn"].forward;
        let reverse_factor = EDGE_WEIGHTS["nim_import"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let mut name_to_defs: FxHashMap<String, Vec<_>> = FxHashMap::default();
        for f in &frags {
            for name in extract_defs(&f.content) {
                name_to_defs
                    .entry(name.to_lowercase())
                    .or_default()
                    .push(f.id.clone());
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            let self_defs = extract_defs(&f.content);
            for imp in extract_imports(&f.content) {
                base::link_by_name(&f.id, &imp, &idx, &mut edges, import_w, reverse_factor);
            }
            for cap in CALL_RE.captures_iter(&f.content) {
                let name = &cap[1];
                if self_defs.contains(name) || NIM_KEYWORDS.contains(name) {
                    continue;
                }
                let w = if name.starts_with(|c: char| c.is_uppercase()) {
                    type_w
                } else {
                    fn_w
                };
                if let Some(targets) = name_to_defs.get(&name.to_lowercase()) {
                    for t in targets {
                        if t != &f.id {
                            add_edge(&mut edges, &f.id, t, w, reverse_factor);
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
        let nim_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_nim_file(f)).collect();
        if nim_changed.is_empty() {
            return vec![];
        }
        let mut refs = FxHashSet::default();
        for f in &nim_changed {
            if let Some(content) = base::read_file_cached(f, file_cache) {
                refs.extend(extract_imports(&content));
            }
        }
        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
