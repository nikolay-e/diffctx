use std::path::{Path, PathBuf};
use std::sync::Arc;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::edge_weights::{C_FAMILY_SEMANTIC, SEMANTIC_DISCOVERY};
use crate::config::extensions::C_FAMILY_EXTENSIONS;
use crate::config::weights::EDGE_WEIGHTS;
use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge};

fn is_c_family(path: &Path) -> bool {
    let ext = base::file_ext(path);
    C_FAMILY_EXTENSIONS.contains(ext.as_str())
}

static HEADER_EXTENSIONS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [".h", ".hpp", ".hh", ".hxx", ".h++"]
        .iter()
        .copied()
        .collect()
});

static IMPL_EXTENSIONS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [".c", ".cpp", ".cc", ".cxx", ".c++", ".m", ".mm"]
        .iter()
        .copied()
        .collect()
});

static INCLUDE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^\s*#\s*(?:include|import)\s*[<"]([^>"]+)[>"]"#).unwrap());
static FUNC_CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b(\w+)\s*\(").unwrap());
static TYPE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w*)\b").unwrap());
static FUNC_DEF_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*(?:[\w*&]+\s+)+(\w+)\s*\(").unwrap());
static TYPE_DEF_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*(?:class|struct|enum|union|typedef)\s+([A-Z]\w*)").unwrap());
static INHERITANCE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?:class|struct)\s+(\w+)\s*:\s*(?:public|protected|private)?\s*(\w+)").unwrap()
});

static C_KEYWORDS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [
        "if",
        "for",
        "while",
        "switch",
        "case",
        "return",
        "sizeof",
        "typeof",
        "alignof",
        "static_assert",
        "do",
        "else",
        "goto",
        "break",
        "continue",
        "default",
        "register",
        "volatile",
        "extern",
        "typedef",
        "auto",
        "inline",
        "restrict",
        "noexcept",
        "decltype",
        "nullptr",
        "throw",
        "try",
        "catch",
        "delete",
        "new",
        "template",
        "namespace",
        "using",
        "operator",
    ]
    .iter()
    .copied()
    .collect()
});

static C_COMMON_MACROS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [
        "NULL", "TRUE", "FALSE", "BOOL", "DWORD", "HANDLE", "VOID", "HRESULT", "LPCTSTR", "LPCSTR",
        "LPWSTR", "INT", "UINT", "LONG", "ULONG", "WORD", "BYTE", "CHAR", "SHORT", "EOF",
        "SIZE_MAX", "INT_MAX", "INT_MIN",
    ]
    .iter()
    .copied()
    .collect()
});

fn extract_includes(content: &str) -> FxHashSet<String> {
    let mut includes = FxHashSet::default();
    for cap in INCLUDE_RE.captures_iter(content) {
        let header = cap[1].to_string();
        if header.contains('/') {
            includes.insert(header.split('/').next_back().unwrap().to_string());
        }
        includes.insert(header);
    }
    includes
}

fn extract_definitions(content: &str) -> (FxHashSet<String>, FxHashSet<String>) {
    let functions: FxHashSet<String> = FUNC_DEF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .filter(|n| {
            !C_KEYWORDS.contains(n.as_str()) && n.len() > SEMANTIC_DISCOVERY.min_identifier_length
        })
        .collect();
    let types: FxHashSet<String> = TYPE_DEF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect();
    (functions, types)
}

fn extract_references(
    content: &str,
    own_defs: &FxHashSet<String>,
) -> (FxHashSet<String>, FxHashSet<String>) {
    let calls: FxHashSet<String> = FUNC_CALL_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .filter(|n| {
            !C_KEYWORDS.contains(n.as_str())
                && !own_defs.contains(n)
                && !n.starts_with('_')
                && n.len() > SEMANTIC_DISCOVERY.min_identifier_length
        })
        .collect();
    let type_refs: FxHashSet<String> = TYPE_REF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .filter(|n| {
            !C_COMMON_MACROS.contains(n.as_str())
                && !own_defs.contains(n)
                && n.len() > SEMANTIC_DISCOVERY.min_identifier_length
        })
        .collect();
    (calls, type_refs)
}

pub struct CFamilyEdgeBuilder;

impl EdgeBuilder for CFamilyEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let c_frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_c_family(Path::new(f.path())))
            .collect();
        if c_frags.is_empty() {
            return FxHashMap::default();
        }

        let include_weight = EDGE_WEIGHTS["c_include"].forward;
        let call_weight = EDGE_WEIGHTS["c_call"].forward;
        let type_weight = EDGE_WEIGHTS["c_type"].forward;
        let inheritance_weight = EDGE_WEIGHTS["c_inheritance"].forward;
        let reverse_factor = EDGE_WEIGHTS["c_include"].reverse_factor;
        let base_weight = C_FAMILY_SEMANTIC.base_weight;

        // An include names a file, so its edge lands on the file's
        // representative fragment (base::file_representatives — the sibling
        // builder's long-standing semantics; see its doc on the accepted
        // reachability trade, #208). Buckets therefore hold file paths, and
        // ambiguity is measured in files: a basename repeated across the tree
        // is what makes an include unresolvable.
        // Representatives keyed off the full slice: is_c_family is per-path, so
        // the map restricted to C-family paths is identical, and only C-family
        // paths are ever looked up.
        let reps = super::super::base::file_representatives(fragments);
        let mut header_to_files: FxHashMap<String, (Vec<Arc<str>>, FxHashSet<Arc<str>>)> =
            FxHashMap::default();
        let mut func_defs_map: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut func_def_files: FxHashMap<String, FxHashSet<Arc<str>>> = FxHashMap::default();
        let mut type_defs_map: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut type_def_files: FxHashMap<String, FxHashSet<Arc<str>>> = FxHashMap::default();
        let mut frag_own_defs: FxHashMap<FragmentId, FxHashSet<String>> = FxHashMap::default();

        // Order-preserving dedup: the Vec keeps insertion order (edge emission
        // order depends on it), the set makes membership O(1) instead of a
        // linear rescan per fragment of every file sharing the key.
        let push_file_key = |map: &mut FxHashMap<String, (Vec<Arc<str>>, FxHashSet<Arc<str>>)>,
                             key: String,
                             f: &Fragment| {
            let (order, seen) = map.entry(key).or_default();
            if seen.insert(f.id.path.clone()) {
                order.push(f.id.path.clone());
            }
        };

        for f in &c_frags {
            let path = Path::new(f.path());
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();

            push_file_key(&mut header_to_files, name, f);
            if !stem.is_empty() {
                push_file_key(&mut header_to_files, format!("{stem}.h"), f);
                push_file_key(&mut header_to_files, format!("{stem}.hpp"), f);
            }

            let (functions, types) = extract_definitions(&f.content);
            let mut own_defs = FxHashSet::default();
            for func in &functions {
                func_defs_map
                    .entry(func.clone())
                    .or_default()
                    .push(f.id.clone());
                func_def_files
                    .entry(func.clone())
                    .or_default()
                    .insert(f.id.path.clone());
                own_defs.insert(func.clone());
            }
            for t in &types {
                type_defs_map
                    .entry(t.clone())
                    .or_default()
                    .push(f.id.clone());
                type_def_files
                    .entry(t.clone())
                    .or_default()
                    .insert(f.id.path.clone());
                own_defs.insert(t.clone());
            }
            frag_own_defs.insert(f.id.clone(), own_defs);
        }

        let max_files = C_FAMILY_SEMANTIC.max_files_per_name;
        let unambiguous = |files: Option<&FxHashSet<Arc<str>>>| {
            files.map(|s| s.len() <= max_files).unwrap_or(true)
        };

        let mut edges: EdgeDict = FxHashMap::default();

        for (i, f) in c_frags.iter().enumerate() {
            // The envoy shape (520 files sharing one stem) made a single
            // c_family build outrun the whole timeout; the between-builders
            // check cannot interrupt it, so poll inside the loop (#210).
            crate::deadline::check_current_every(i, 256, "edge construction (c_family)");
            for inc in extract_includes(&f.content) {
                let inc_name = if inc.contains('/') {
                    inc.split('/').next_back().unwrap().to_string()
                } else {
                    inc.clone()
                };
                let Some((candidates, _)) = header_to_files.get(&inc_name) else {
                    continue;
                };
                // `#include "common/buffer/buffer_impl.h"` names one file; a
                // candidate qualifies only if its path actually ends with the
                // include's components. A bare `#include "config.h"` cannot be
                // disambiguated that way, so it falls back to the basename
                // bucket — bounded below, since envoy carries 256 `config.h`
                // and linking to all of them is noise at quadratic cost.
                let suffix = format!("/{inc}");
                let matched: Vec<&Arc<str>> = if inc.contains('/') {
                    candidates
                        .iter()
                        .filter(|p| p.as_ref() == inc || p.ends_with(&suffix))
                        .collect()
                } else {
                    candidates.iter().collect()
                };
                if matched.is_empty() || matched.len() > max_files {
                    continue;
                }
                for path in matched {
                    let Some(rep) = reps.get(path.as_ref()) else {
                        continue;
                    };
                    if rep != &f.id {
                        add_edge(&mut edges, &f.id, rep, include_weight, reverse_factor);
                    }
                }
            }

            let own_defs = frag_own_defs.get(&f.id).cloned().unwrap_or_default();
            let (calls, type_refs) = extract_references(&f.content, &own_defs);

            for call in &calls {
                if !unambiguous(func_def_files.get(call)) {
                    continue;
                }
                for def_id in func_defs_map.get(call).unwrap_or(&vec![]) {
                    if def_id != &f.id {
                        add_edge(&mut edges, &f.id, def_id, call_weight, reverse_factor);
                    }
                }
            }

            for t in &type_refs {
                if !unambiguous(type_def_files.get(t)) {
                    continue;
                }
                for def_id in type_defs_map.get(t).unwrap_or(&vec![]) {
                    if def_id != &f.id {
                        add_edge(&mut edges, &f.id, def_id, type_weight, reverse_factor);
                    }
                }
            }

            for cap in INHERITANCE_RE.captures_iter(&f.content) {
                let base = cap[2].to_string();
                if !unambiguous(type_def_files.get(&base)) {
                    continue;
                }
                for def_id in type_defs_map.get(&base).unwrap_or(&vec![]) {
                    if def_id != &f.id {
                        add_edge(
                            &mut edges,
                            &f.id,
                            def_id,
                            inheritance_weight,
                            reverse_factor,
                        );
                    }
                }
            }
        }

        // Header/impl pairing is scoped to the directory, the same call made
        // for bare-stem discovery in c6694261: co-location is the rule's
        // justification, and applied tree-wide it degenerates on exactly the
        // stems real projects repeat most (envoy: 520 files with stem
        // `config`, whose fragment-level cross product alone was tens of
        // millions of edges).
        let mut by_stem: FxHashMap<(String, String), (Vec<&str>, FxHashSet<&str>)> =
            FxHashMap::default();
        for f in &c_frags {
            let path = Path::new(f.path());
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let dir = path
                .parent()
                .map(|d| d.to_string_lossy().to_string())
                .unwrap_or_default();
            let (order, seen) = by_stem.entry((dir, stem)).or_default();
            if seen.insert(f.path()) {
                order.push(f.path());
            }
        }

        // A header/impl pair is a relation between two files; representatives
        // carry it (see base::file_representatives on the accepted
        // reachability trade, #208). Pairing every fragment with every
        // fragment restated the same fact quadratically.
        for (i, (_key, (files, _))) in by_stem.iter().enumerate() {
            crate::deadline::check_current_every(i, 256, "edge construction (c_family pairing)");
            if files.len() < 2 {
                continue;
            }
            let headers: Vec<&&str> = files
                .iter()
                .filter(|p| HEADER_EXTENSIONS.contains(base::file_ext(Path::new(**p)).as_str()))
                .collect();
            let impls: Vec<&&str> = files
                .iter()
                .filter(|p| IMPL_EXTENSIONS.contains(base::file_ext(Path::new(**p)).as_str()))
                .collect();
            for h in &headers {
                for imp in &impls {
                    if let (Some(hr), Some(ir)) = (reps.get(**h), reps.get(**imp)) {
                        add_edge(&mut edges, hr, ir, base_weight, reverse_factor);
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
        _repo_root: Option<&Path>,
        _file_cache: Option<&FxHashMap<PathBuf, String>>,
    ) -> Vec<PathBuf> {
        let c_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_c_family(f)).collect();
        if c_changed.is_empty() {
            return vec![];
        }

        let changed_set: FxHashSet<PathBuf> = changed.iter().cloned().collect();
        let mut discovered: FxHashSet<PathBuf> = FxHashSet::default();
        let mut frontier: Vec<PathBuf> = c_changed.iter().map(|f| (*f).clone()).collect();

        for _ in 0..SEMANTIC_DISCOVERY.max_depth {
            let mut hop_found: Vec<PathBuf> = Vec::new();

            let mut included_headers: FxHashSet<String> = FxHashSet::default();
            for f in &frontier {
                if let Ok(content) = std::fs::read_to_string(f) {
                    included_headers.extend(extract_includes(&content));
                }
            }

            let mut changed_names: FxHashSet<String> = FxHashSet::default();
            for f in &frontier {
                if let Some(name) = f.file_name() {
                    changed_names.insert(name.to_string_lossy().to_string());
                }
                if let Some(stem) = f.file_stem() {
                    let s = stem.to_string_lossy().to_string();
                    changed_names.insert(format!("{}.h", s));
                    changed_names.insert(format!("{}.hpp", s));
                }
            }

            for candidate in candidates {
                if changed_set.contains(candidate)
                    || discovered.contains(candidate)
                    || !is_c_family(candidate)
                {
                    continue;
                }
                let cand_name = candidate
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if included_headers.contains(&cand_name) {
                    hop_found.push(candidate.clone());
                    continue;
                }
                if let Ok(content) = std::fs::read_to_string(candidate) {
                    let cand_includes = extract_includes(&content);
                    for inc in &cand_includes {
                        let inc_name = if inc.contains('/') {
                            inc.split('/').next_back().unwrap().to_string()
                        } else {
                            inc.clone()
                        };
                        if changed_names.contains(&inc_name) {
                            hop_found.push(candidate.clone());
                            break;
                        }
                    }
                }
            }

            let new_files: Vec<PathBuf> = hop_found
                .into_iter()
                .filter(|f| !discovered.contains(f))
                .collect();
            if new_files.is_empty() {
                break;
            }
            discovered.extend(new_files.iter().cloned());
            frontier = new_files;
        }

        let mut result: Vec<PathBuf> = discovered.into_iter().collect();
        result.sort();
        result
    }
}
