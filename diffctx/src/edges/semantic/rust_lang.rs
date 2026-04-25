use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::extensions::RUST_EXTENSIONS;
use crate::config::weights::EDGE_WEIGHTS;
use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, add_edges_from_ids};

fn is_rust_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    RUST_EXTENSIONS.contains(ext.as_str())
}

static USE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*use\s+([\w:]+(?:::\{[^}]+\})?)").unwrap());
static MOD_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*(?:pub\s+)?mod\s+(\w+)\s*[;{]").unwrap());
static TYPE_DEF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|trait|type|union)\s+([A-Z]\w*)")
        .unwrap()
});
static FN_DEF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([a-z_]\w*)").unwrap()
});
static TYPE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w*)\b").unwrap());
static FN_CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([a-z_]\w+)\s*[(<]").unwrap());
static PATH_CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b(\w+)::(\w+)").unwrap());
static IMPL_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*impl(?:<[^>]*>)?\s+(\w+)\s+for\s+(\w+)").unwrap());
static PUB_USE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*pub\s+use\s+([\w:]+)").unwrap());

static RUST_KEYWORDS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [
        "if",
        "else",
        "for",
        "while",
        "loop",
        "match",
        "return",
        "break",
        "continue",
        "let",
        "mut",
        "ref",
        "fn",
        "pub",
        "mod",
        "use",
        "struct",
        "enum",
        "trait",
        "impl",
        "type",
        "where",
        "as",
        "in",
        "self",
        "super",
        "crate",
        "extern",
        "async",
        "await",
        "move",
        "unsafe",
        "const",
        "static",
        "dyn",
        "box",
        "true",
        "false",
        "Some",
        "None",
        "Ok",
        "Err",
        "vec",
        "println",
        "eprintln",
        "format",
        "write",
        "writeln",
        "panic",
        "assert",
        "assert_eq",
        "assert_ne",
        "debug_assert",
        "todo",
        "unimplemented",
        "unreachable",
        "cfg",
        "derive",
        "allow",
        "deny",
        "warn",
    ]
    .iter()
    .copied()
    .collect()
});

fn extract_uses(content: &str) -> FxHashSet<String> {
    USE_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_mods(content: &str) -> FxHashSet<String> {
    MOD_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_definitions(content: &str) -> (FxHashSet<String>, FxHashSet<String>) {
    let funcs: FxHashSet<String> = FN_DEF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect();
    let types: FxHashSet<String> = TYPE_DEF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect();
    (funcs, types)
}

fn extract_trait_impls(content: &str) -> Vec<(String, String)> {
    IMPL_RE
        .captures_iter(content)
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .collect()
}

fn extract_pub_uses(content: &str) -> Vec<String> {
    PUB_USE_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_references(
    content: &str,
) -> (
    FxHashSet<String>,
    FxHashSet<String>,
    FxHashSet<(String, String)>,
) {
    let type_refs: FxHashSet<String> = TYPE_REF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .filter(|n| !RUST_KEYWORDS.contains(n.as_str()))
        .collect();
    let fn_calls: FxHashSet<String> = FN_CALL_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .filter(|n| !RUST_KEYWORDS.contains(n.as_str()))
        .collect();
    let path_calls: FxHashSet<(String, String)> = PATH_CALL_RE
        .captures_iter(content)
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .collect();
    (type_refs, fn_calls, path_calls)
}

fn stem_to_mod_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if stem == "mod" || stem == "lib" {
        path.parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or(stem)
    } else {
        stem
    }
}

const DISCOVERY_MAX_DEPTH: usize = 2;

pub struct RustEdgeBuilder;

impl EdgeBuilder for RustEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let rust_frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_rust_file(Path::new(f.path())))
            .collect();
        if rust_frags.is_empty() {
            return FxHashMap::default();
        }

        let mod_weight = EDGE_WEIGHTS["rust_mod"].forward;
        let use_weight = EDGE_WEIGHTS["rust_use"].forward;
        let type_weight = EDGE_WEIGHTS["rust_type"].forward;
        let fn_weight = EDGE_WEIGHTS["rust_fn"].forward;
        let same_crate_weight = EDGE_WEIGHTS["rust_same_crate"].forward;
        let reverse_factor = EDGE_WEIGHTS["rust_mod"].reverse_factor;

        let mut name_to_frags: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut mod_to_frags: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut type_defs: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut fn_defs: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut trait_impls: FxHashMap<FragmentId, Vec<(String, String)>> = FxHashMap::default();

        for f in &rust_frags {
            let path = Path::new(f.path());
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            name_to_frags
                .entry(stem.clone())
                .or_default()
                .push(f.id.clone());

            if stem == "mod" || stem == "lib" {
                if let Some(parent_name) = path.parent().and_then(|p| p.file_name()) {
                    mod_to_frags
                        .entry(parent_name.to_string_lossy().to_lowercase())
                        .or_default()
                        .push(f.id.clone());
                }
            } else {
                mod_to_frags.entry(stem).or_default().push(f.id.clone());
            }

            let (funcs, types) = extract_definitions(&f.content);
            for t in types {
                type_defs
                    .entry(t.to_lowercase())
                    .or_default()
                    .push(f.id.clone());
            }
            for func in funcs {
                fn_defs
                    .entry(func.to_lowercase())
                    .or_default()
                    .push(f.id.clone());
            }

            for mod_name in extract_mods(&f.content) {
                mod_to_frags
                    .entry(mod_name.to_lowercase())
                    .or_default()
                    .push(f.id.clone());
            }

            let impls = extract_trait_impls(&f.content);
            if !impls.is_empty() {
                trait_impls.insert(f.id.clone(), impls);
            }

            for pub_use_path in extract_pub_uses(&f.content) {
                let leaf_lower = pub_use_path.split("::").last().unwrap_or("").to_lowercase();
                if !leaf_lower.is_empty() && !name_to_frags.contains_key(&leaf_lower) {
                    let has_target = type_defs
                        .get(&leaf_lower)
                        .map_or(false, |v| v.iter().any(|fid| fid != &f.id))
                        || fn_defs
                            .get(&leaf_lower)
                            .map_or(false, |v| v.iter().any(|fid| fid != &f.id));
                    if has_target {
                        name_to_frags
                            .entry(leaf_lower)
                            .or_default()
                            .push(f.id.clone());
                    }
                }
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for (impl_fid, pairs) in &trait_impls {
            for (trait_name, _type_name) in pairs {
                for trait_fid in type_defs.get(&trait_name.to_lowercase()).unwrap_or(&vec![]) {
                    if trait_fid != impl_fid {
                        add_edge(&mut edges, impl_fid, trait_fid, type_weight, reverse_factor);
                    }
                }
            }
        }

        for f in &rust_frags {
            for pub_use_path in extract_pub_uses(&f.content) {
                let leaf_lower = pub_use_path.split("::").last().unwrap_or("").to_lowercase();
                for target_list in [type_defs.get(&leaf_lower), fn_defs.get(&leaf_lower)]
                    .iter()
                    .flatten()
                {
                    for target_fid in *target_list {
                        if target_fid != &f.id {
                            add_edge(&mut edges, &f.id, target_fid, use_weight, reverse_factor);
                        }
                    }
                }
            }
        }

        for rf in &rust_frags {
            let (type_refs, fn_calls, path_calls) = extract_references(&rf.content);

            for use_path in extract_uses(&rf.content) {
                for part in use_path.split("::") {
                    let part_lower = part.to_lowercase();
                    add_edges_from_ids(
                        &mut edges,
                        &rf.id,
                        mod_to_frags.get(&part_lower).unwrap_or(&vec![]),
                        use_weight,
                        reverse_factor,
                    );
                    add_edges_from_ids(
                        &mut edges,
                        &rf.id,
                        name_to_frags.get(&part_lower).unwrap_or(&vec![]),
                        use_weight,
                        reverse_factor,
                    );
                }
            }

            for mod_name in extract_mods(&rf.content) {
                for fid in name_to_frags
                    .get(&mod_name.to_lowercase())
                    .unwrap_or(&vec![])
                {
                    if fid != &rf.id {
                        add_edge(&mut edges, &rf.id, fid, mod_weight, reverse_factor);
                    }
                }
            }

            for type_ref in &type_refs {
                for fid in type_defs.get(&type_ref.to_lowercase()).unwrap_or(&vec![]) {
                    if fid != &rf.id {
                        add_edge(&mut edges, &rf.id, fid, type_weight, reverse_factor);
                    }
                }
            }

            for fn_call in &fn_calls {
                for fid in fn_defs.get(&fn_call.to_lowercase()).unwrap_or(&vec![]) {
                    if fid != &rf.id {
                        add_edge(&mut edges, &rf.id, fid, fn_weight, reverse_factor);
                    }
                }
            }

            for (mod_name, _symbol) in &path_calls {
                for fid in mod_to_frags
                    .get(&mod_name.to_lowercase())
                    .unwrap_or(&vec![])
                {
                    if fid != &rf.id {
                        add_edge(&mut edges, &rf.id, fid, use_weight, reverse_factor);
                    }
                }
            }

            let stem = Path::new(rf.path())
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if stem == "lib" || stem == "mod" {
                let parent_dir = Path::new(rf.path()).parent();
                for other in &rust_frags {
                    if let Some(pd) = parent_dir {
                        if Path::new(other.path()).parent() == Some(pd) && other.id != rf.id {
                            add_edge(
                                &mut edges,
                                &rf.id,
                                &other.id,
                                same_crate_weight,
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
        _repo_root: Option<&Path>,
        file_cache: Option<&FxHashMap<PathBuf, String>>,
    ) -> Vec<PathBuf> {
        let rust_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_rust_file(f)).collect();
        if rust_changed.is_empty() {
            return vec![];
        }

        let rust_candidates: Vec<PathBuf> = candidates
            .iter()
            .filter(|c| is_rust_file(c))
            .cloned()
            .collect();

        let mut mod_name_to_files: FxHashMap<String, Vec<PathBuf>> = FxHashMap::default();
        let mut file_uses: FxHashMap<PathBuf, FxHashSet<String>> = FxHashMap::default();
        let mut file_mods: FxHashMap<PathBuf, FxHashSet<String>> = FxHashMap::default();

        for candidate in &rust_candidates {
            mod_name_to_files
                .entry(stem_to_mod_name(candidate))
                .or_default()
                .push(candidate.clone());
            if let Some(content) = base::read_file_cached(candidate, file_cache) {
                file_uses.insert(candidate.clone(), extract_uses(&content));
                file_mods.insert(candidate.clone(), extract_mods(&content));
            }
        }

        let changed_set: FxHashSet<PathBuf> = changed.iter().cloned().collect();
        let mut discovered: FxHashSet<PathBuf> = FxHashSet::default();
        let mut frontier: FxHashSet<PathBuf> = rust_changed.iter().map(|f| (*f).clone()).collect();

        for _ in 0..DISCOVERY_MAX_DEPTH {
            let skip: FxHashSet<PathBuf> = changed_set.union(&discovered).cloned().collect();
            let frontier_mod_names: FxHashSet<String> =
                frontier.iter().map(|f| stem_to_mod_name(f)).collect();

            let mut forward_targets: FxHashSet<String> = FxHashSet::default();
            for f in &frontier {
                if let Some(uses) = file_uses.get(f) {
                    for use_path in uses {
                        for part in use_path.split("::") {
                            forward_targets.insert(part.to_lowercase());
                        }
                    }
                }
                if let Some(mods) = file_mods.get(f) {
                    for m in mods {
                        forward_targets.insert(m.to_lowercase());
                    }
                }
            }

            let mut found: FxHashSet<PathBuf> = FxHashSet::default();

            for target_name in &forward_targets {
                for candidate in mod_name_to_files.get(target_name).unwrap_or(&vec![]) {
                    if !skip.contains(candidate) && !found.contains(candidate) {
                        found.insert(candidate.clone());
                    }
                }
            }

            for candidate in &rust_candidates {
                if skip.contains(candidate) || found.contains(candidate) {
                    continue;
                }
                let cand_mods = file_mods.get(candidate).cloned().unwrap_or_default();
                if !cand_mods.is_disjoint(&frontier_mod_names) {
                    found.insert(candidate.clone());
                    continue;
                }
                if let Some(uses) = file_uses.get(candidate) {
                    let use_parts: FxHashSet<String> = uses
                        .iter()
                        .flat_map(|p| p.split("::").map(|s| s.to_lowercase()))
                        .collect();
                    if !use_parts.is_disjoint(&frontier_mod_names) {
                        found.insert(candidate.clone());
                    }
                }
            }

            if found.is_empty() {
                break;
            }
            discovered.extend(found.iter().cloned());
            frontier = found;
        }

        let mut result: Vec<PathBuf> = discovered.into_iter().collect();
        result.sort();
        result
    }
}
