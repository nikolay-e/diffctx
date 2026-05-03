use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge_unidirectional, path_to_module};

static IMPORT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?:from\s+([\w.]+)\s+import|import\s+([\w.]+))").unwrap());

fn is_python_test(name: &str) -> bool {
    name.starts_with("test_") || name.ends_with("_test.py")
}

fn is_js_test(name: &str, path_str: &str) -> bool {
    name.contains(".test.") || name.contains(".spec.") || path_str.contains("__tests__")
}

fn is_rust_test(name: &str, path_str: &str) -> bool {
    path_str.contains("/tests/") || name == "tests.rs"
}

fn is_jvm_test(name: &str) -> bool {
    let stem = if let Some(idx) = name.rfind('.') {
        &name[..idx]
    } else {
        name
    };
    let lower = stem.to_lowercase();
    lower.ends_with("test") || lower.starts_with("test")
}

fn is_test_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let path_str = path.to_string_lossy().to_lowercase();
    let ext = base::file_ext(path);

    let lang_match = match ext.as_str() {
        ".py" => is_python_test(&name),
        ".js" | ".ts" | ".jsx" | ".tsx" => is_js_test(&name, &path_str),
        ".rs" => is_rust_test(&name, &path_str),
        ".java" | ".kt" | ".kts" | ".scala" => is_jvm_test(&name),
        _ => false,
    };
    lang_match || path_str.contains("/tests/") || path_str.contains("/test/")
}

fn extract_imports(content: &str) -> FxHashSet<String> {
    let mut imports = FxHashSet::default();
    for cap in IMPORT_RE.captures_iter(content) {
        if let Some(m) = cap.get(1) {
            imports.insert(m.as_str().to_string());
        }
        if let Some(m) = cap.get(2) {
            imports.insert(m.as_str().to_string());
        }
    }
    imports
}

fn has_direct_import(test_imports: &FxHashSet<String>, src_module: &str) -> bool {
    if src_module.is_empty() {
        return false;
    }
    let suffix = format!(".{}", src_module);
    test_imports
        .iter()
        .any(|imp| imp == src_module || imp.ends_with(&suffix))
}

fn extract_target_name_from_test(test_name: &str) -> Option<String> {
    let lower = test_name.to_lowercase();
    if lower.starts_with("test_") {
        return Some(lower[5..].to_string());
    }
    if lower.ends_with("_test") {
        return Some(lower[..lower.len() - 5].to_string());
    }
    if lower.contains(".test") {
        return Some(lower.split(".test").next()?.to_string());
    }
    if lower.contains(".spec") {
        return Some(lower.split(".spec").next()?.to_string());
    }
    if test_name.starts_with("Test")
        && test_name.len() > 4
        && test_name.as_bytes()[4].is_ascii_uppercase()
    {
        return Some(test_name[4..].to_lowercase());
    }
    if test_name.ends_with("Tests") && test_name.len() > 5 {
        return Some(test_name[..test_name.len() - 5].to_lowercase());
    }
    if test_name.ends_with("Test") && test_name.len() > 4 {
        return Some(test_name[..test_name.len() - 4].to_lowercase());
    }
    None
}

pub struct TestEdgeBuilder;

impl EdgeBuilder for TestEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let weight_direct = EDGE_WEIGHTS["test_direct"].forward;
        let weight_naming = EDGE_WEIGHTS["test_naming"].forward;
        let test_reverse_weight = EDGE_WEIGHTS["test_reverse"].forward;

        let mut test_frags: Vec<&Fragment> = Vec::new();
        let mut by_base: FxHashMap<String, Vec<&Fragment>> = FxHashMap::default();

        for f in fragments {
            let path = Path::new(f.path());
            if is_test_file(path) {
                test_frags.push(f);
            } else {
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                by_base.entry(stem).or_default().push(f);
            }
        }

        let mut module_cache: FxHashMap<String, String> = FxHashMap::default();
        for src_list in by_base.values() {
            for sf in src_list {
                let path_str = sf.path().to_string();
                module_cache
                    .entry(path_str)
                    .or_insert_with_key(|_| path_to_module(Path::new(sf.path()), repo_root));
            }
        }

        let mut import_cache: FxHashMap<String, FxHashSet<String>> = FxHashMap::default();
        for tf in &test_frags {
            let path_str = tf.path().to_string();
            import_cache
                .entry(path_str)
                .or_insert_with(|| extract_imports(&tf.content));
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for test_frag in &test_frags {
            let test_stem = Path::new(test_frag.path())
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let target_name = match extract_target_name_from_test(&test_stem) {
                Some(name) => name,
                None => continue,
            };

            let test_imports = import_cache
                .get(test_frag.path())
                .cloned()
                .unwrap_or_default();

            for src_frag in by_base.get(&target_name).unwrap_or(&vec![]) {
                let src_module = module_cache
                    .get(src_frag.path())
                    .map(|s| s.as_str())
                    .unwrap_or("");
                let weight = if has_direct_import(&test_imports, src_module) {
                    weight_direct
                } else {
                    weight_naming
                };

                add_edge_unidirectional(&mut edges, &test_frag.id, &src_frag.id, weight);
                add_edge_unidirectional(
                    &mut edges,
                    &src_frag.id,
                    &test_frag.id,
                    test_reverse_weight,
                );
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
        let changed_set: FxHashSet<PathBuf> = changed.iter().cloned().collect();
        let mut candidate_by_stem: FxHashMap<String, Vec<PathBuf>> = FxHashMap::default();
        for c in candidates {
            if !changed_set.contains(c) {
                let stem = c
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                candidate_by_stem.entry(stem).or_default().push(c.clone());
            }
        }

        let mut discovered: Vec<PathBuf> = Vec::new();

        for changed_file in changed {
            let ext = base::file_ext(changed_file);
            let stem = changed_file
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();

            if is_test_file(changed_file) {
                if let Some(target) = extract_target_name_from_test(&stem) {
                    for c in candidate_by_stem.get(&target).unwrap_or(&vec![]) {
                        if base::file_ext(c) == ext {
                            discovered.push(c.clone());
                        }
                    }
                }
            } else {
                let stem_lower = stem.to_lowercase();
                for test_stem in [
                    format!("test_{}", stem_lower),
                    format!("{}_test", stem_lower),
                ] {
                    for c in candidate_by_stem.get(&test_stem).unwrap_or(&vec![]) {
                        if base::file_ext(c) == ext && is_test_file(c) {
                            discovered.push(c.clone());
                        }
                    }
                }

                if matches!(
                    ext.as_str(),
                    ".js" | ".ts" | ".jsx" | ".tsx" | ".mjs" | ".cjs"
                ) {
                    let stem_test = format!("{stem_lower}.test");
                    let stem_spec = format!("{stem_lower}.spec");
                    for c in candidate_by_stem.get(&stem_test).unwrap_or(&vec![]) {
                        if base::file_ext(c) == ext && is_test_file(c) {
                            discovered.push(c.clone());
                        }
                    }
                    for c in candidate_by_stem.get(&stem_spec).unwrap_or(&vec![]) {
                        if base::file_ext(c) == ext && is_test_file(c) {
                            discovered.push(c.clone());
                        }
                    }
                }
            }
        }

        discovered
    }

    fn category_label(&self) -> Option<&str> {
        Some("test_edge")
    }
}
