use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::extensions::{JS_TS_EXTENSIONS, TYPESCRIPT_EXTENSIONS};
use crate::config::weights::LANG_WEIGHTS;
use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder};

fn is_js_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    JS_TS_EXTENSIONS.contains(ext.as_str())
}

fn is_ts_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    TYPESCRIPT_EXTENSIONS.contains(ext.as_str())
}

static IMPORT_SOURCE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?m)(?:import\s+.*?\s+from\s+['"]([^'"]+)['"]|require\s*\(\s*['"]([^'"]+)['"]\s*\))"#,
    )
    .unwrap()
});
static EXPORT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*export\s+(?:default\s+)?(?:function|class|const|let|var|interface|type|enum|abstract\s+class)\s+([A-Za-z_$]\w*)").unwrap()
});
static NAMED_IMPORT_NAMES_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)import\s*\{([^}]+)\}\s*from").unwrap());
static CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Za-z_$]\w*)\s*\(").unwrap());
static TYPE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w*)\b").unwrap());
static DEF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:export\s+)?(?:default\s+)?(?:function|class|const|let|var|interface|type|enum)\s+([A-Za-z_$]\w*)").unwrap()
});

static JS_KEYWORDS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [
        "if",
        "for",
        "while",
        "return",
        "function",
        "class",
        "const",
        "let",
        "var",
        "new",
        "delete",
        "typeof",
        "instanceof",
        "void",
        "switch",
        "case",
        "break",
        "continue",
        "throw",
        "try",
        "catch",
        "finally",
        "yield",
        "async",
        "await",
        "import",
        "export",
        "default",
        "from",
        "require",
        "super",
        "this",
        "true",
        "false",
        "null",
        "undefined",
        "console",
        "Math",
        "Object",
        "Array",
        "String",
        "Number",
        "Boolean",
        "Error",
        "Promise",
        "Map",
        "Set",
        "Date",
        "JSON",
        "RegExp",
        "Symbol",
        "parseInt",
        "parseFloat",
        "setTimeout",
        "setInterval",
        "clearTimeout",
        "clearInterval",
    ]
    .iter()
    .copied()
    .collect()
});

fn extract_import_sources(content: &str) -> FxHashSet<String> {
    let mut sources = FxHashSet::default();
    for cap in IMPORT_SOURCE_RE.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            sources.insert(m.as_str().to_string());
        }
        if let Some(m) = cap.get(2) {
            sources.insert(m.as_str().to_string());
        }
    }
    sources
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
        .filter(|n| !JS_KEYWORDS.contains(n.as_str()))
        .collect()
}

fn extract_type_refs(content: &str) -> FxHashSet<String> {
    TYPE_REF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_exports(content: &str) -> FxHashSet<String> {
    let mut exported = FxHashSet::default();
    for cap in EXPORT_RE.captures_iter(content) {
        exported.insert(cap[1].to_lowercase());
    }
    exported
}

fn resolve_relative_import(
    src_path: &Path,
    import_source: &str,
    known_paths: &FxHashSet<PathBuf>,
) -> Option<PathBuf> {
    if !import_source.starts_with('.') {
        return None;
    }
    let base = src_path.parent()?;
    let candidate_base = base.join(import_source);
    let candidate_base = candidate_base.as_path();

    for ext in JS_TS_EXTENSIONS.iter() {
        let with_ext = candidate_base.with_extension(&ext[1..]);
        if known_paths.contains(&with_ext) {
            return Some(with_ext);
        }
    }

    for index_name in &["index.ts", "index.tsx", "index.js", "index.jsx"] {
        let idx = candidate_base.join(index_name);
        if known_paths.contains(&idx) {
            return Some(idx);
        }
    }

    None
}

const IMPORT_WEIGHT: f64 = 0.55;
const REVERSE_FACTOR: f64 = 0.5;

pub struct JavaScriptEdgeBuilder;

impl EdgeBuilder for JavaScriptEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let js_frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_js_file(Path::new(f.path())))
            .collect();
        if js_frags.is_empty() {
            return FxHashMap::default();
        }

        let mut name_to_defs: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut frag_defines: FxHashMap<FragmentId, FxHashSet<String>> = FxHashMap::default();

        for f in &js_frags {
            let defines = extract_defines(&f.content);
            for name in &defines {
                name_to_defs
                    .entry(name.clone())
                    .or_default()
                    .push(f.id.clone());
            }
            frag_defines.insert(f.id.clone(), defines);
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &js_frags {
            let self_defs = frag_defines.get(&f.id).cloned().unwrap_or_default();
            let is_ts = is_ts_file(Path::new(f.path()));
            let w = if is_ts {
                LANG_WEIGHTS.get("typescript").expect("ts weights")
            } else {
                LANG_WEIGHTS.get("javascript").expect("js weights")
            };

            let calls = extract_calls(&f.content);
            let type_refs = extract_type_refs(&f.content);

            for (ref_set, base_weight) in [(&calls, w.call), (&type_refs, w.type_ref)] {
                for name in ref_set {
                    if self_defs.contains(name) {
                        continue;
                    }
                    if let Some(dst_ids) = name_to_defs.get(name) {
                        for dst_id in dst_ids {
                            if dst_id == &f.id {
                                continue;
                            }
                            base::add_edge(&mut edges, &f.id, dst_id, base_weight, REVERSE_FACTOR);
                        }
                    }
                }
            }
        }

        let mut file_to_frags: FxHashMap<PathBuf, Vec<FragmentId>> = FxHashMap::default();
        for f in &js_frags {
            file_to_frags
                .entry(PathBuf::from(f.path()))
                .or_default()
                .push(f.id.clone());
        }
        let fragment_paths: FxHashSet<PathBuf> = file_to_frags.keys().cloned().collect();

        for f in &js_frags {
            let src_path = PathBuf::from(f.path());
            let import_sources = extract_import_sources(&f.content);
            for source in &import_sources {
                if !source.starts_with('.') {
                    continue;
                }
                if let Some(resolved) = resolve_relative_import(&src_path, source, &fragment_paths)
                {
                    if resolved == src_path {
                        continue;
                    }
                    if let Some(target_ids) = file_to_frags.get(&resolved) {
                        if let Some(src_ids) = file_to_frags.get(&src_path) {
                            for src_id in src_ids {
                                for tgt_id in target_ids {
                                    if tgt_id != src_id {
                                        base::add_edge(
                                            &mut edges,
                                            src_id,
                                            tgt_id,
                                            IMPORT_WEIGHT,
                                            REVERSE_FACTOR,
                                        );
                                    }
                                }
                            }
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
        _file_cache: Option<&FxHashMap<PathBuf, String>>,
    ) -> Vec<PathBuf> {
        let js_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_js_file(f)).collect();
        if js_changed.is_empty() {
            return vec![];
        }

        let changed_set: FxHashSet<PathBuf> = changed.iter().cloned().collect();
        let mut discovered: FxHashSet<PathBuf> = FxHashSet::default();
        let mut frontier: Vec<PathBuf> = js_changed.iter().map(|f| (*f).clone()).collect();

        for _ in 0..2 {
            let mut newly_found: FxHashSet<PathBuf> = FxHashSet::default();

            let mut changed_names: FxHashSet<String> = FxHashSet::default();
            for f in &frontier {
                let stem = f
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                changed_names.insert(stem.clone());
                if stem == "index" {
                    if let Some(parent) = f.parent() {
                        if let Some(name) = parent.file_name() {
                            changed_names.insert(name.to_string_lossy().to_lowercase());
                        }
                    }
                }
                if let Some(root) = repo_root {
                    if let Ok(rel) = f.strip_prefix(root) {
                        let rel_str = rel.with_extension("").to_string_lossy().replace('\\', "/");
                        changed_names.insert(rel_str);
                    }
                }
            }

            for candidate in candidates {
                if changed_set.contains(candidate)
                    || discovered.contains(candidate)
                    || !is_js_file(candidate)
                {
                    continue;
                }
                if let Ok(content) = std::fs::read_to_string(candidate) {
                    let imports = extract_import_sources(&content);
                    for imp in &imports {
                        let imp_lower = imp.to_lowercase();
                        if changed_names.iter().any(|n| {
                            n.len() >= 3
                                && (imp_lower.contains(n.as_str())
                                    || imp_lower.ends_with(n.as_str()))
                        }) {
                            newly_found.insert(candidate.clone());
                            break;
                        }
                    }
                }
            }

            let exported_names: FxHashSet<String> = frontier
                .iter()
                .filter_map(|f| std::fs::read_to_string(f).ok())
                .flat_map(|c| extract_exports(&c))
                .collect();

            if !exported_names.is_empty() {
                for candidate in candidates {
                    if changed_set.contains(candidate)
                        || discovered.contains(candidate)
                        || newly_found.contains(candidate)
                        || !is_js_file(candidate)
                    {
                        continue;
                    }
                    if let Ok(content) = std::fs::read_to_string(candidate) {
                        for cap in NAMED_IMPORT_NAMES_RE.captures_iter(&content) {
                            let names: FxHashSet<String> = cap[1]
                                .split(',')
                                .filter_map(|n| {
                                    let trimmed =
                                        n.trim().split(" as ").next()?.trim().to_lowercase();
                                    if trimmed.is_empty() {
                                        None
                                    } else {
                                        Some(trimmed)
                                    }
                                })
                                .collect();
                            if !names.is_disjoint(&exported_names) {
                                newly_found.insert(candidate.clone());
                                break;
                            }
                        }
                    }
                }
            }

            let candidate_set: FxHashSet<PathBuf> = candidates.iter().cloned().collect();
            for f in &frontier {
                if let Ok(content) = std::fs::read_to_string(f) {
                    let sources = extract_import_sources(&content);
                    for source in sources {
                        if source.starts_with('.') {
                            if let Some(resolved) =
                                resolve_relative_import(f, &source, &candidate_set)
                            {
                                if !changed_set.contains(&resolved)
                                    && !discovered.contains(&resolved)
                                {
                                    newly_found.insert(resolved);
                                }
                            }
                        }
                    }
                }
            }

            if newly_found.is_empty() {
                break;
            }
            discovered.extend(newly_found.iter().cloned());
            frontier = newly_found.into_iter().filter(|f| is_js_file(f)).collect();
        }

        let mut result: Vec<PathBuf> = discovered.into_iter().collect();
        result.sort();
        result
    }
}
