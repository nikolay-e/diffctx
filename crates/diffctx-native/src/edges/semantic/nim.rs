use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edges_from_ids};

fn is_nim_file(path: &Path) -> bool {
    base::has_ext(path, &[".nim", ".nims"])
}

// One line only: across a newline the read was `palette\n\nconst width` (#285).
static IMPORT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^[ \t]*import[ \t]+([^\n#]+)").unwrap());
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
    base::kw(concat!(
        "if elif else when case of for while block break continue return result proc func method ",
        "var let const type import from include export template macro iterator converter object ref ",
        "ptr nil true false and or not xor div mod echo assert doAssert len add del new newSeq ",
    ))
});
/// The module paths a file imports, as written: `import a/b, c`, the
/// bracket form `import a/[b, c]`, `import x as y`, `from a/b import c`,
/// `include a/b`. The standard library (`std/…`) is no file of the repo.
fn extract_imports(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();
    for cap in IMPORT_RE.captures_iter(content) {
        let line = cap[1].trim();
        let mut parts: Vec<String> = Vec::new();
        let mut depth = 0;
        let mut cur = String::new();
        for c in line.chars() {
            match c {
                '[' => depth += 1,
                ']' => depth -= 1,
                ',' if depth == 0 => {
                    parts.push(std::mem::take(&mut cur));
                    continue;
                }
                _ => {}
            }
            cur.push(c);
        }
        parts.push(cur);
        for part in parts {
            let part = part.split(" as ").next().unwrap_or("").trim().to_string();
            match part.split_once('[') {
                Some((prefix, rest)) => {
                    for member in rest.trim_end_matches(']').split(',') {
                        refs.insert(format!("{prefix}{}", member.trim()));
                    }
                }
                None => {
                    refs.insert(part);
                }
            }
        }
    }
    refs.extend(
        FROM_IMPORT_RE
            .captures_iter(content)
            .map(|c| c[1].to_string()),
    );
    refs.extend(INCLUDE_RE.captures_iter(content).map(|c| c[1].to_string()));
    refs.retain(|r| !r.is_empty() && !r.starts_with("std/") && !r.starts_with("pkg/"));
    refs
}

const NIM_SUFFIXES: &[&str] = &[".nim"];

fn extract_defs(content: &str) -> FxHashSet<String> {
    let mut defs: FxHashSet<String> = base::captures1(&PROC_RE, content).collect();
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
        let Some(frags) = base::frags_where(fragments, |f| is_nim_file(Path::new(f.path()))) else {
            return FxHashMap::default();
        };

        let import_w = EDGE_WEIGHTS["nim_import"].forward;
        let type_w = EDGE_WEIGHTS["nim_type"].forward;
        let fn_w = EDGE_WEIGHTS["nim_fn"].forward;
        let reverse_factor = EDGE_WEIGHTS["nim_import"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let mut name_to_defs: FxHashMap<String, Vec<_>> = FxHashMap::default();
        for f in &frags {
            base::index_lower(&mut name_to_defs, extract_defs(&f.content), &f.id);
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            let self_defs = extract_defs(&f.content);
            for imp in extract_imports(&f.content) {
                base::link_module_path(
                    &f.id,
                    &imp,
                    NIM_SUFFIXES,
                    &idx,
                    &mut edges,
                    import_w,
                    reverse_factor,
                );
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
                    add_edges_from_ids(&mut edges, &f.id, &targets, w, reverse_factor);
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
        base::discover_by_module_paths(
            changed,
            candidates,
            repo_root,
            file_cache,
            is_nim_file,
            extract_imports,
            NIM_SUFFIXES,
        )
    }
}
