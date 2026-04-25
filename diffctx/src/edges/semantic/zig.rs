use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, discover_files_by_refs};

fn is_zig_file(path: &Path) -> bool {
    base::file_ext(path) == ".zig"
}

static IMPORT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"@import\s*\(\s*['"]([^'"]+)['"]"#).unwrap());
static FN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*(?:pub\s+)?fn\s+(\w+)").unwrap());
static STRUCT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:pub\s+)?const\s+(\w+)\s*=\s*(?:struct|union|enum|packed struct)")
        .unwrap()
});
static TYPE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w+)\b").unwrap());
static CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b(\w+)\s*\(").unwrap());

static ZIG_KEYWORDS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [
        "if",
        "else",
        "while",
        "for",
        "switch",
        "return",
        "break",
        "continue",
        "fn",
        "pub",
        "const",
        "var",
        "struct",
        "enum",
        "union",
        "error",
        "try",
        "catch",
        "unreachable",
        "undefined",
        "null",
        "true",
        "false",
        "comptime",
        "inline",
        "extern",
        "export",
        "test",
        "defer",
        "errdefer",
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

fn extract_defs(content: &str) -> FxHashSet<String> {
    let mut defs: FxHashSet<String> = FN_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect();
    defs.extend(STRUCT_RE.captures_iter(content).map(|c| c[1].to_string()));
    defs
}

pub struct ZigEdgeBuilder;

impl EdgeBuilder for ZigEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_zig_file(Path::new(f.path())))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let import_w = EDGE_WEIGHTS["zig_import"].forward;
        let type_w = EDGE_WEIGHTS["zig_type"].forward;
        let fn_w = EDGE_WEIGHTS["zig_fn"].forward;
        let reverse_factor = EDGE_WEIGHTS["zig_import"].reverse_factor;

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
            for cap in TYPE_RE.captures_iter(&f.content) {
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
            for cap in CALL_RE.captures_iter(&f.content) {
                let name = &cap[1];
                if self_defs.contains(name) || ZIG_KEYWORDS.contains(name) {
                    continue;
                }
                if let Some(targets) = name_to_defs.get(&name.to_lowercase()) {
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
        let zig_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_zig_file(f)).collect();
        if zig_changed.is_empty() {
            return vec![];
        }
        let mut refs = FxHashSet::default();
        for f in &zig_changed {
            if let Some(content) = base::read_file_cached(f, file_cache) {
                refs.extend(extract_imports(&content));
            }
        }
        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
