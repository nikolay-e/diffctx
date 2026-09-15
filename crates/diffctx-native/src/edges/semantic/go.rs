use std::path::{Path, PathBuf};

/// Same ambiguity bar as `CFamilySemanticWeights::max_files_per_name`.
const MAX_FILES_PER_NAME: usize = 8;
const MAX_REFERENCING_FILES_UNCONFIRMED: usize = 64;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::edge_weights::{GO_SEMANTIC, SEMANTIC_DISCOVERY};
use crate::config::extensions::GO_EXTENSIONS;
use crate::config::weights::EDGE_WEIGHTS;
use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, add_edges_from_ids};

fn is_go_file(path: &Path) -> bool {
    GO_EXTENSIONS.contains(base::file_ext(path).as_str())
}

static IMPORT_LINE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"^\s*(?:\w+\s+|\.\s+|_\s+)?"([^"]+)""#).unwrap());
static PACKAGE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*package\s+(\w+)").unwrap());
static TYPE_DEF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*type\s+([A-Z]\w*)").unwrap());
static FUNC_DEF_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*func\s+(?:\([^)]*\)\s+)?([A-Z]\w*)\s*\(").unwrap());
static FUNC_CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w*)\s*\(").unwrap());
static TYPE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w*)\b").unwrap());
static PKG_CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([a-z]\w+)\.([A-Z]\w*)").unwrap());
static INIT_FUNC_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*func\s+init\s*\(").unwrap());

/// Only what the `import` statements say — `import "x"` and the lines of
/// an `import (` block. The reader used to take every line that started
/// with a quoted string as an import, which on a test-heavy tree made a
/// string literal a package.
fn extract_imports(content: &str) -> FxHashSet<String> {
    let mut imports = FxHashSet::default();
    let mut in_block = false;
    for line in content.lines() {
        let t = line.trim_start();
        if in_block {
            if t.starts_with(')') {
                in_block = false;
            } else if let Some(c) = IMPORT_LINE_RE.captures(line) {
                imports.insert(c[1].to_string());
            }
            continue;
        }
        let Some(rest) = t.strip_prefix("import") else {
            continue;
        };
        let rest = rest.trim_start();
        if rest.starts_with('(') {
            in_block = true;
        } else if let Some(c) = IMPORT_LINE_RE.captures(rest) {
            imports.insert(c[1].to_string());
        }
    }
    imports
}

/// None when the fragment carries no `package` line — i.e. every
/// function-body fragment. The old `"main"` fallback funneled all of them
/// into one bucket, and the same-package loop cross-linked essentially the
/// whole Go universe through it: 103.8M edges on a gitpod-scale monorepo
/// (#116).
fn get_package_name(content: &str) -> Option<String> {
    PACKAGE_RE.captures(content).map(|c| c[1].to_string())
}

fn extract_definitions(content: &str) -> (FxHashSet<String>, FxHashSet<String>) {
    let funcs: FxHashSet<String> = base::captures1(&FUNC_DEF_RE, content).collect();
    let types: FxHashSet<String> = base::captures1(&TYPE_DEF_RE, content).collect();
    (funcs, types)
}

struct References {
    /// Bare `Name(`: a function of this package or a dot-import, resolved
    /// across files. `pkg.Name(` resolves through `pkg_calls` and
    /// `recv.Name(` through `method_calls`; letting either match here linked
    /// every `NewClient(` to every `func NewClient` in reach.
    func_calls: FxHashSet<String>,
    /// `recv.Name(`: a method on a receiver the reader cannot type, resolved
    /// within the calling package only.
    method_calls: FxHashSet<String>,
    /// Bare `Name` in type position; `pkg.Name` is a `pkg_calls` entry and a
    /// `type Name` line is the definition, not a reference.
    type_refs: FxHashSet<String>,
    pkg_calls: FxHashSet<(String, String)>,
}

fn extract_references(content: &str) -> References {
    let mut func_calls = FxHashSet::default();
    let mut method_calls = FxHashSet::default();
    for c in FUNC_CALL_RE.captures_iter(content) {
        let Some(m) = c.get(1) else { continue };
        let before = content[..m.start()].trim_end();
        if before.ends_with("func") {
            continue;
        }
        if before.ends_with('.') {
            method_calls.insert(m.as_str().to_string());
        } else {
            func_calls.insert(m.as_str().to_string());
        }
    }
    let type_refs: FxHashSet<String> = TYPE_REF_RE
        .captures_iter(content)
        .filter_map(|c| {
            let m = c.get(1)?;
            let before = content[..m.start()].trim_end();
            (!before.ends_with('.') && !before.ends_with("type")).then(|| m.as_str().to_string())
        })
        .collect();
    let pkg_calls: FxHashSet<(String, String)> = PKG_CALL_RE
        .captures_iter(content)
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .collect();
    References {
        func_calls,
        method_calls,
        type_refs,
        pkg_calls,
    }
}

fn has_init_func(content: &str) -> bool {
    INIT_FUNC_RE.is_match(content)
}

/// Distinct files referencing each defined name, counted to one past the
/// cap so the pass is bounded by names × cap.
fn count_referencing_files<'a>(
    go_frags: &[&'a Fragment],
    def_files: &FxHashMap<String, FxHashSet<&str>>,
) -> FxHashMap<String, usize> {
    let mut seen: FxHashMap<&str, FxHashSet<&'a str>> = FxHashMap::default();
    for gf in go_frags {
        let refs = extract_references(&gf.content);
        for name in refs.type_refs.iter().chain(refs.func_calls.iter()) {
            let lower = name.to_lowercase();
            let Some((key, _)) = def_files.get_key_value(&lower) else {
                continue;
            };
            let files = seen.entry(key.as_str()).or_default();
            if files.len() <= MAX_REFERENCING_FILES_UNCONFIRMED {
                files.insert(gf.path());
            }
        }
    }
    seen.into_iter()
        .map(|(name, files)| (name.to_string(), files.len()))
        .collect()
}

pub struct GoEdgeBuilder;

impl GoEdgeBuilder {
    fn build_indices<'a>(
        &self,
        go_frags: &'a [&'a Fragment],
        repo_root: Option<&Path>,
    ) -> (
        FxHashMap<String, Vec<FragmentId>>,
        FxHashMap<String, Vec<FragmentId>>,
        FxHashMap<String, Vec<FragmentId>>,
        FxHashMap<String, Vec<FragmentId>>,
        FxHashMap<String, FxHashSet<&'a str>>,
        FxHashMap<String, FxHashSet<&'a str>>,
    ) {
        // A package and a directory are file-level relations: they resolve
        // to each file's representative fragment, as every other file-level
        // edge does (#208), never to each fragment of a generated
        // thousand-fragment file — that fan-out was 14.3M edges on a
        // kubernetes-scale commit (#196).
        let reps = base::file_representatives(go_frags.iter().copied());
        let mut pkg_to_frags: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut path_to_frags: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut type_defs: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut func_defs: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut def_files: FxHashMap<String, FxHashSet<&str>> = FxHashMap::default();
        let mut pkg_files: FxHashMap<String, FxHashSet<&str>> = FxHashMap::default();

        for f in go_frags {
            let is_rep = reps.get(f.path()) == Some(&f.id);
            if let Some(pkg) = get_package_name(&f.content) {
                let lower = pkg.to_lowercase();
                pkg_files.entry(lower.clone()).or_default().insert(f.path());
                if is_rep {
                    pkg_to_frags.entry(lower).or_default().push(f.id.clone());
                }
            }

            if let Some(root) = repo_root {
                if let Ok(rel) = Path::new(f.path()).strip_prefix(root) {
                    if let Some(parent) = rel.parent() {
                        if is_rep {
                            path_to_frags
                                .entry(parent.to_string_lossy().to_string())
                                .or_default()
                                .push(f.id.clone());
                        }
                    }
                }
            }

            let (funcs, types) = extract_definitions(&f.content);
            for t in types {
                let lower = t.to_lowercase();
                def_files.entry(lower.clone()).or_default().insert(f.path());
                type_defs.entry(lower).or_default().push(f.id.clone());
            }
            for func in funcs {
                let lower = func.to_lowercase();
                def_files.entry(lower.clone()).or_default().insert(f.path());
                func_defs.entry(lower).or_default().push(f.id.clone());
            }
        }

        (
            pkg_to_frags,
            path_to_frags,
            type_defs,
            func_defs,
            def_files,
            pkg_files,
        )
    }
}

impl EdgeBuilder for GoEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let Some(go_frags) = base::frags_where(fragments, |f| is_go_file(Path::new(f.path())))
        else {
            return FxHashMap::default();
        };

        let import_weight = EDGE_WEIGHTS["go_import"].forward;
        let type_weight = EDGE_WEIGHTS["go_type"].forward;
        let func_weight = EDGE_WEIGHTS["go_func"].forward;
        let same_package_weight = EDGE_WEIGHTS["go_same_package"].forward;
        let reverse_factor = EDGE_WEIGHTS["go_import"].reverse_factor;
        let init_same_package_weight = GO_SEMANTIC.init_same_package_weight;

        let (pkg_to_frags, path_to_frags, type_defs, func_defs, def_files, pkg_files) =
            self.build_indices(&go_frags, repo_root);
        let reps = base::file_representatives(go_frags.iter().copied());
        let init_files: FxHashSet<&str> = go_frags
            .iter()
            .filter(|f| has_init_func(&f.content))
            .map(|f| f.path())
            .collect();
        let pkg_of_frag: FxHashMap<&str, String> = go_frags
            .iter()
            .filter_map(|f| get_package_name(&f.content).map(|p| (f.path(), p.to_lowercase())))
            .collect();

        let name_capped = |name: &str| {
            def_files
                .get(name)
                .is_some_and(|s| s.len() > MAX_FILES_PER_NAME)
        };
        let pkg_capped = |name: &str| {
            pkg_files
                .get(name)
                .is_some_and(|s| s.len() > MAX_FILES_PER_NAME)
        };

        let mut edges: EdgeDict = FxHashMap::default();

        let mut path_last_comp: FxHashMap<&str, Vec<&String>> = FxHashMap::default();
        for path_str in path_to_frags.keys() {
            if let Some(last) = path_str.rsplit('/').next() {
                path_last_comp.entry(last).or_default().push(path_str);
            }
        }

        // Imports are a property of the file, carried by its representative
        // on the source side as on the target side.
        let mut file_imports: FxHashMap<&str, FxHashSet<String>> = FxHashMap::default();
        for gf in &go_frags {
            let found = extract_imports(&gf.content);
            if !found.is_empty() {
                file_imports.entry(gf.path()).or_default().extend(found);
            }
        }
        let file_import_pkgs: FxHashMap<&str, FxHashSet<String>> = file_imports
            .iter()
            .map(|(path, imps)| {
                let pkgs = imps
                    .iter()
                    .map(|i| i.rsplit('/').next().unwrap_or(i).to_lowercase())
                    .collect();
                (*path, pkgs)
            })
            .collect();
        let referencing_files = count_referencing_files(&go_frags, &def_files);
        let confirmed = |src_path: &str, dst: &FragmentId| {
            let Some(dst_pkg) = pkg_of_frag.get(dst.path.as_ref()) else {
                return false;
            };
            pkg_of_frag.get(src_path) == Some(dst_pkg)
                || file_import_pkgs
                    .get(src_path)
                    .is_some_and(|p| p.contains(dst_pkg))
        };

        let mut reported = 0u64;
        for (i, gf) in go_frags.iter().enumerate() {
            if !crate::resource::poll_emissions(i, 256, edges.len() as u64, &mut reported) {
                break;
            }
            let is_rep = reps.get(gf.path()) == Some(&gf.id);
            let References {
                func_calls,
                method_calls,
                type_refs,
                pkg_calls,
            } = extract_references(&gf.content);
            let no_imports = FxHashSet::default();
            let imports = if is_rep {
                file_imports.get(gf.path()).unwrap_or(&no_imports)
            } else {
                &no_imports
            };

            for imp in imports {
                let imp_pkg = imp.split('/').next_back().unwrap_or(imp).to_lowercase();
                // Direct lookup: the previous full-map scan was
                // O(imports x packages) and cost ~40s alone on a
                // kubernetes-scale commit (#196).
                if !pkg_capped(&imp_pkg) {
                    if let Some(frag_ids) = pkg_to_frags.get(&imp_pkg) {
                        add_edges_from_ids(
                            &mut edges,
                            &gf.id,
                            frag_ids,
                            import_weight,
                            reverse_factor,
                        );
                    }
                }
                // Last-component index instead of a full scan: the old form
                // was O(imports x dirs) with two String allocations per probe
                // and stood at ~39s alone on a kubernetes commit (#196). A
                // dir can only match if its last component appears as a
                // component of the import, so only that posting list is
                // verified against the full predicate.
                for part in imp.split('/') {
                    let Some(dirs) = path_last_comp.get(part) else {
                        continue;
                    };
                    for path_str in dirs {
                        if *imp == **path_str
                            || imp.ends_with(&format!("/{}", path_str))
                            || imp.contains(&format!("/{}/", path_str))
                        {
                            if let Some(frag_ids) = path_to_frags.get(*path_str) {
                                add_edges_from_ids(
                                    &mut edges,
                                    &gf.id,
                                    frag_ids,
                                    import_weight,
                                    reverse_factor,
                                );
                            }
                        }
                    }
                }
            }

            // A name used in more files than the cap without importing its
            // definer's package is vocabulary (the python reader's bar, #196);
            // an import-confirmed reference is never capped.
            for (refs, defs, weight) in [
                (&type_refs, &type_defs, type_weight),
                (&func_calls, &func_defs, func_weight),
            ] {
                for name in refs {
                    let lower = name.to_lowercase();
                    if name_capped(&lower) {
                        continue;
                    }
                    let hub = referencing_files.get(&lower).copied().unwrap_or(0)
                        > MAX_REFERENCING_FILES_UNCONFIRMED;
                    for fid in defs.get(&lower).unwrap_or(&vec![]) {
                        if fid == &gf.id || (hub && !confirmed(gf.path(), fid)) {
                            continue;
                        }
                        add_edge(&mut edges, &gf.id, fid, weight, reverse_factor);
                    }
                }
            }

            let own_pkg = pkg_of_frag.get(gf.path());
            for method in &method_calls {
                let lower = method.to_lowercase();
                if name_capped(&lower) {
                    continue;
                }
                for fid in func_defs.get(&lower).unwrap_or(&vec![]) {
                    if fid != &gf.id && pkg_of_frag.get(fid.path.as_ref()) == own_pkg {
                        add_edge(&mut edges, &gf.id, fid, func_weight, reverse_factor);
                    }
                }
            }

            // `pkg.Symbol` names one definition when the package has it;
            // only a package without that symbol endorses the package as a
            // set of files.
            for (pkg_name, symbol) in &pkg_calls {
                let lower = pkg_name.to_lowercase();
                if pkg_capped(&lower) {
                    continue;
                }
                let symbol_lower = symbol.to_lowercase();
                let mut named = false;
                for defs in [func_defs.get(&symbol_lower), type_defs.get(&symbol_lower)]
                    .into_iter()
                    .flatten()
                {
                    for fid in defs {
                        if fid != &gf.id
                            && pkg_of_frag
                                .get(fid.path.as_ref())
                                .is_some_and(|p| *p == lower)
                        {
                            add_edge(&mut edges, &gf.id, fid, func_weight, reverse_factor);
                            named = true;
                        }
                    }
                }
                if named {
                    continue;
                }
                add_edges_from_ids(
                    &mut edges,
                    &gf.id,
                    pkg_to_frags.get(&lower).unwrap_or(&vec![]),
                    func_weight,
                    reverse_factor,
                );
            }

            // Sharing a package is a relation between files: one edge per
            // file pair through the representatives, weighted up when the
            // file carries an `init`.
            if reps.get(gf.path()) != Some(&gf.id) {
                continue;
            }
            let sp_weight = if init_files.contains(gf.path()) {
                init_same_package_weight
            } else {
                same_package_weight
            };
            if let Some(current_pkg) = pkg_of_frag.get(gf.path()) {
                for fid in pkg_to_frags.get(current_pkg).unwrap_or(&vec![]) {
                    if fid != &gf.id {
                        add_edge(&mut edges, &gf.id, fid, sp_weight, reverse_factor);
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
        let go_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_go_file(f)).collect();
        if go_changed.is_empty() {
            return vec![];
        }

        let changed_set: FxHashSet<PathBuf> = changed.iter().cloned().collect();
        let go_candidates: Vec<PathBuf> = candidates
            .iter()
            .filter(|c| !changed_set.contains(*c) && is_go_file(c))
            .cloned()
            .collect();

        let mut discovered: FxHashSet<PathBuf> = FxHashSet::default();

        let pkg_dirs: FxHashSet<PathBuf> = go_changed
            .iter()
            .filter_map(|f| f.parent().map(|p| p.to_path_buf()))
            .collect();
        for c in &go_candidates {
            if let Some(parent) = c.parent() {
                if pkg_dirs.contains(&parent.to_path_buf()) {
                    discovered.insert(c.clone());
                }
            }
        }

        let mut candidate_index: FxHashMap<PathBuf, (String, FxHashSet<String>)> =
            FxHashMap::default();
        for c in &go_candidates {
            let content = base::read_file_cached(c, file_cache);
            if let Some(content) = content {
                let Some(pkg) = get_package_name(&content).map(|p| p.to_lowercase()) else {
                    continue;
                };
                let imports = extract_imports(&content);
                candidate_index.insert(c.clone(), (pkg, imports));
            }
        }

        let mut frontier: FxHashSet<PathBuf> = go_changed.iter().map(|f| (*f).clone()).collect();

        for _ in 0..SEMANTIC_DISCOVERY.max_depth {
            let mut next_frontier: FxHashSet<PathBuf> = FxHashSet::default();
            for f in &frontier {
                let content = base::read_file_cached(f, file_cache);
                if let Some(content) = content {
                    let f_imports = extract_imports(&content);
                    let Some(f_pkg) = get_package_name(&content).map(|p| p.to_lowercase()) else {
                        continue;
                    };

                    for c in &go_candidates {
                        if changed_set.contains(c) || discovered.contains(c) {
                            continue;
                        }
                        if let Some((c_pkg, c_imports)) = candidate_index.get(c) {
                            let forward_match = f_imports.iter().any(|imp| {
                                imp.split('/').next_back().unwrap_or(imp).to_lowercase() == *c_pkg
                            });
                            let reverse_match = c_imports.iter().any(|imp| {
                                imp.split('/').next_back().unwrap_or(imp).to_lowercase() == f_pkg
                            });
                            if forward_match || reverse_match {
                                discovered.insert(c.clone());
                                next_frontier.insert(c.clone());
                            }
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
