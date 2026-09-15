use std::path::{Path, PathBuf};

/// Same ambiguity bar as `CFamilySemanticWeights::max_files_per_name`: a name
/// defined in more files than this is vocabulary, not a dependency, and is
/// skipped outright rather than truncated.
const MAX_FILES_PER_NAME: usize = 8;
/// The same bar on the referencing side, for edges no import confirms: a
/// name used in more files than this without importing its definer (`hass`,
/// `config`, `entry` — a fixture, a parameter, a word) is vocabulary too.
/// On a 10k-module monorepo 37 such names carried 43 % of all identifier
/// contributions and 168 names over 512 files carried 71 % (#196).
const MAX_REFERENCING_FILES_UNCONFIRMED: usize = 64;

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
    PYTHON_EXTENSIONS.contains(base::file_ext(path).as_str())
}

static IMPORT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*import\s+([\w.]+)").unwrap());
static FROM_IMPORT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*from\s+([\w.]+)\s+import\s+(.+)").unwrap());
static CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Za-z_]\w*)\s*\(").unwrap());
static TYPE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w*)\b").unwrap());
static DEF_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*(?:def|class|async\s+def)\s+([A-Za-z_]\w*)").unwrap());

/// What a file imports: every module it names (and each package on the
/// way, for `import a.b.c`) plus, for `from m import x, y`, the names it
/// takes from `m` — the edge layer links those to their definitions and the
/// bare module to its representative fragment.
#[derive(Default)]
struct Imports {
    modules: FxHashSet<String>,
    named: Vec<(String, Vec<String>)>,
}

fn extract_imports(content: &str, path: &Path, repo_root: Option<&Path>) -> Imports {
    let mut imports = Imports::default();
    for cap in IMPORT_RE.captures_iter(content) {
        add_module_chain(&mut imports.modules, &cap[1]);
    }
    for cap in FROM_IMPORT_RE.captures_iter(content) {
        let Some(module) = resolve_from_module(&cap[1], path, repo_root) else {
            continue;
        };
        add_module_chain(&mut imports.modules, &module);
        let names: Vec<String> = cap[2]
            .trim_end_matches('\\')
            .trim_matches(|c| c == '(' || c == ')' || c == ' ')
            .split(',')
            .filter_map(|n| n.split_whitespace().next())
            .filter(|n| *n != "*" && !n.is_empty())
            .map(str::to_string)
            .collect();
        imports.named.push((module, names));
    }
    imports
}

fn add_module_chain(modules: &mut FxHashSet<String>, module: &str) {
    let parts: Vec<&str> = module.split('.').collect();
    for i in 1..=parts.len() {
        modules.insert(parts[..i].join("."));
    }
}

/// `.x` is a sibling module of the file's package, `..x` one package up,
/// a bare `.` the package itself. The reader used to resolve every relative
/// import to the package alone, so `from .coordinator import C` never
/// confirmed the `coordinator` edge it names.
fn resolve_from_module(spec: &str, path: &Path, repo_root: Option<&Path>) -> Option<String> {
    if !spec.starts_with('.') {
        return Some(spec.to_string());
    }
    let ups = spec.chars().take_while(|c| *c == '.').count();
    let rest = &spec[ups..];
    let mut package = path.parent()?;
    for _ in 1..ups {
        package = package.parent()?;
    }
    let base = path_to_module(package, repo_root);
    Some(match (base.is_empty(), rest.is_empty()) {
        (true, true) => return None,
        (true, false) => rest.to_string(),
        (false, true) => base,
        (false, false) => format!("{base}.{rest}"),
    })
}

fn extract_defines(content: &str) -> FxHashSet<String> {
    base::captures1(&DEF_RE, content).collect()
}

fn extract_calls(content: &str) -> FxHashSet<String> {
    base::captures1(&CALL_RE, content)
        .filter(|n| !PY_KEYWORDS.contains(n.as_str()))
        .collect()
}

fn extract_type_refs(content: &str) -> FxHashSet<String> {
    base::captures1(&TYPE_REF_RE, content).collect()
}

static PY_KEYWORDS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    base::kw(concat!(
        "if for while return def class import from as with try except finally raise pass break ",
        "continue yield lambda assert del elif else global nonlocal and or not is in async await ",
        "True False None print len range type list dict set tuple str int float bool super ",
        "isinstance hasattr getattr setattr property staticmethod classmethod ",
    ))
});
/// Distinct files that reference each defined name outside their own
/// definitions, counted only up to one past the cap so the pass stays
/// bounded by names × cap rather than by references.
fn count_referencing_files<'a>(
    py_frags: &[&'a Fragment],
    name_to_defs: &FxHashMap<String, Vec<FragmentId>>,
    frag_defines: &FxHashMap<FragmentId, FxHashSet<String>>,
) -> FxHashMap<String, usize> {
    let mut seen: FxHashMap<&str, FxHashSet<&'a str>> = FxHashMap::default();
    for f in py_frags {
        let self_defs = frag_defines.get(&f.id);
        let referenced = extract_calls(&f.content)
            .into_iter()
            .chain(f.identifiers.iter().cloned())
            .chain(extract_type_refs(&f.content));
        for name in referenced {
            if self_defs.is_some_and(|d| d.contains(&name)) {
                continue;
            }
            let Some((key, _)) = name_to_defs.get_key_value(&name) else {
                continue;
            };
            let files = seen.entry(key.as_str()).or_default();
            if files.len() <= MAX_REFERENCING_FILES_UNCONFIRMED {
                files.insert(f.path());
            }
        }
    }
    seen.into_iter()
        .map(|(name, files)| (name.to_string(), files.len()))
        .collect()
}

/// Per module: its representative fragment and, for each name defined in
/// it, the tightest fragment defining that name (a class over its methods'
/// enclosing file, a method over the class that contains it).
type ModuleTargets = (
    FxHashMap<String, FragmentId>,
    FxHashMap<String, FxHashMap<String, FragmentId>>,
);

fn module_targets(
    py_frags: &[&Fragment],
    frag_defines: &FxHashMap<FragmentId, FxHashSet<String>>,
    repo_root: Option<&Path>,
) -> ModuleTargets {
    let reps_by_path = base::file_representatives(py_frags.iter().copied());
    let mut reps: FxHashMap<String, FragmentId> = FxHashMap::default();
    let mut defs: FxHashMap<String, FxHashMap<String, FragmentId>> = FxHashMap::default();
    for f in py_frags {
        let module = path_to_module(Path::new(f.path()), repo_root);
        if module.is_empty() {
            continue;
        }
        if let Some(rep) = reps_by_path.get(f.path()) {
            reps.entry(module.clone()).or_insert_with(|| rep.clone());
        }
        let Some(defined) = frag_defines.get(&f.id) else {
            continue;
        };
        let span = f.id.end_line.saturating_sub(f.id.start_line);
        let by_name = defs.entry(module).or_default();
        for name in defined {
            let tighter = by_name
                .get(name)
                .is_none_or(|cur| cur.end_line.saturating_sub(cur.start_line) > span);
            if tighter {
                by_name.insert(name.clone(), f.id.clone());
            }
        }
    }
    (reps, defs)
}

pub struct PythonEdgeBuilder;

impl EdgeBuilder for PythonEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let Some(py_frags) = base::frags_where(fragments, |f| is_python_file(Path::new(f.path())))
        else {
            return FxHashMap::default();
        };

        let weights = LANG_WEIGHTS.get("python").expect("python weights");
        let call_weight = weights.call;
        let symbol_ref_weight = weights.symbol_ref;
        let type_ref_weight = weights.type_ref;

        let mut name_to_defs: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut name_def_files: FxHashMap<String, FxHashSet<&str>> = FxHashMap::default();
        let mut frag_defines: FxHashMap<FragmentId, FxHashSet<String>> = FxHashMap::default();

        for f in &py_frags {
            let defines = extract_defines(&f.content);
            for name in &defines {
                name_to_defs
                    .entry(name.clone())
                    .or_default()
                    .push(f.id.clone());
                name_def_files
                    .entry(name.clone())
                    .or_default()
                    .insert(f.path());
            }
            frag_defines.insert(f.id.clone(), defines);
        }

        let frag_imports: FxHashMap<FragmentId, Imports> = py_frags
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

        let referencing_files = count_referencing_files(&py_frags, &name_to_defs, &frag_defines);
        let (module_reps, module_defs) = module_targets(&py_frags, &frag_defines, repo_root);

        let mut edges: EdgeDict = FxHashMap::default();

        let mut reported = 0u64;
        for (i, f) in py_frags.iter().enumerate() {
            // A 10k-module monorepo still emits tens of millions of edges at
            // the 8-file cap (#196); the between-builders check cannot
            // interrupt a single builder, so poll inside the loop — for the
            // deadline and for the contribution cap alike.
            if !crate::resource::poll_emissions(i, 256, edges.len() as u64, &mut reported) {
                break;
            }
            let self_defs = frag_defines.get(&f.id).cloned().unwrap_or_default();
            let no_imports = Imports::default();
            let src_imports = frag_imports.get(&f.id).unwrap_or(&no_imports);

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
                    // Same ambiguity bar as CFamilySemanticWeights::
                    // max_files_per_name: a name defined in more files than
                    // this is vocabulary, not a dependency. Uncapped, this
                    // loop alone emitted 85M edges on a sentry-scale repo
                    // (#116) — every shared identifier fanned out to every
                    // definition site.
                    if name_def_files
                        .get(name)
                        .is_some_and(|s| s.len() > MAX_FILES_PER_NAME)
                    {
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
                                !dst_module.is_empty() && src_imports.modules.contains(dst_module);
                            if !confirmed
                                && referencing_files.get(name).copied().unwrap_or(0)
                                    > MAX_REFERENCING_FILES_UNCONFIRMED
                            {
                                continue;
                            }
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

            // An import is a file-level relation: it endorses the module
            // (its representative fragment, as every other file-level edge
            // does — #208) and, for `from m import x`, the fragment that
            // defines `x`. Linking every fragment of the importing file to
            // every fragment of the imported one was the other half of the
            // 38M-edge instance (#196).
            for imp in &src_imports.modules {
                if let Some(rep) = module_reps.get(imp) {
                    if rep != &f.id {
                        base::add_edge(
                            &mut edges,
                            &f.id,
                            rep,
                            PYTHON_SEMANTIC.import_weight,
                            PYTHON_SEMANTIC.reverse_factor,
                        );
                    }
                }
            }
            for (module, names) in &src_imports.named {
                let Some(defs) = module_defs.get(module) else {
                    continue;
                };
                for name in names {
                    if let Some(tgt) = defs.get(name) {
                        if tgt != &f.id {
                            base::add_edge(
                                &mut edges,
                                &f.id,
                                tgt,
                                PYTHON_SEMANTIC.import_weight,
                                PYTHON_SEMANTIC.reverse_factor,
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
                file_to_imports.insert(f.clone(), extract_imports(&c, f, repo_root).modules);
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
