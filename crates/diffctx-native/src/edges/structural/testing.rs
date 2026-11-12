use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::testfiles::{
    JVM_TEST_SUFFIX_EXTENSIONS, JVM_TEST_SUFFIXES, is_test_file, naming_links, target_name_of_test,
};
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge_unidirectional, path_to_module};

static IMPORT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*(?:from\s+([\w.]+)\s+import|import\s+([\w.]+))").unwrap());

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

pub(crate) struct TestEdgeBuilder;

impl EdgeBuilder for TestEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let weight_direct = EDGE_WEIGHTS["test_direct"].forward;
        let weight_naming = EDGE_WEIGHTS["test_naming"].forward;
        let test_reverse_weight = EDGE_WEIGHTS["test_reverse"].forward;

        let mut files: Vec<&str> = Vec::new();
        let mut seen: FxHashSet<&str> = FxHashSet::default();
        let mut test_frags_by_path: FxHashMap<&str, Vec<&Fragment>> = FxHashMap::default();
        for f in fragments {
            if seen.insert(f.path()) {
                files.push(f.path());
            }
            if is_test_file(Path::new(f.path())) {
                test_frags_by_path.entry(f.path()).or_default().push(f);
            }
        }

        let reps = base::file_representatives(fragments);
        let mut edges: EdgeDict = FxHashMap::default();
        let mut imports_of: FxHashMap<&str, FxHashSet<String>> = FxHashMap::default();

        // The naming convention names a FILE, so the relation is carried by
        // the two files' representatives (base::file_representatives; see its
        // doc on the accepted reachability trade, #208). One pair per
        // (test file, source file), not per fragment pair; which pairs the
        // convention links is `testfiles::naming_links`.
        for (test, source) in naming_links(&files) {
            let (Some(test_rep), Some(src_rep)) = (reps.get(test), reps.get(source)) else {
                continue;
            };
            // Imports come from EVERY fragment of the test file: module-level
            // import statements land in a gap fragment appended AFTER the
            // definitions, so reading only the first fragment (the old
            // import_cache) never saw them and `test_direct` never fired for
            // tree-sitter languages. Read once per test file that produces an
            // edge.
            let test_imports = imports_of.entry(test).or_insert_with(|| {
                test_frags_by_path
                    .get(test)
                    .map(|frs| {
                        frs.iter()
                            .flat_map(|f| extract_imports(&f.content))
                            .collect()
                    })
                    .unwrap_or_default()
            });
            let src_module = path_to_module(Path::new(source), repo_root);
            let weight = if has_direct_import(test_imports, &src_module) {
                weight_direct
            } else {
                weight_naming
            };
            add_edge_unidirectional(&mut edges, test_rep, src_rep, weight);
            add_edge_unidirectional(&mut edges, src_rep, test_rep, test_reverse_weight);
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
                let stem = base::file_stem_lower(c);
                candidate_by_stem.entry(stem).or_default().push(c.clone());
            }
        }

        let mut discovered: Vec<PathBuf> = Vec::new();

        for changed_file in changed {
            let ext = base::file_ext(changed_file);
            let stem = base::file_stem_string(changed_file);

            if is_test_file(changed_file) {
                if let Some(target) = target_name_of_test(&stem) {
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

                if JVM_TEST_SUFFIX_EXTENSIONS.contains(&ext.as_str()) {
                    for suffix in JVM_TEST_SUFFIXES {
                        let test_stem = format!("{stem_lower}{suffix}");
                        for c in candidate_by_stem.get(&test_stem).unwrap_or(&vec![]) {
                            if base::file_ext(c) == ext && is_test_file(c) {
                                discovered.push(c.clone());
                            }
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// #217 B: a test file's imports land in the gap fragment the parser
    /// appends after its definitions, so reading them off the first fragment
    /// left `test_direct` unfired. A same-named test that does not import its
    /// subject stays a naming link.
    #[test]
    fn a_test_that_imports_its_subject_links_by_the_import() {
        let file =
            |path: &str, content: &str| crate::parsers::fragment_file(Arc::from(path), content);
        let test = |import: &str| {
            format!("import pytest\n{import}\n\ndef test_sum():\n    assert total([1, 2]) == 3\n")
        };
        let mut fragments = file(
            "shop/pricing.py",
            "def total(items):\n    return sum(items)\n",
        );
        let importing = file(
            "tests/test_pricing.py",
            &test("from shop.pricing import total\n"),
        );
        assert!(
            !importing[0].content.contains("import"),
            "the first fragment holds the imports, so this measures nothing"
        );
        fragments.extend(importing);
        fragments.extend(file("legacy/test_pricing.py", &test("")));
        let edges = TestEdgeBuilder.build(&fragments, None);
        let reps = base::file_representatives(&fragments);
        let weight = |test: &str| {
            edges
                .get(&(reps[test].clone(), reps["shop/pricing.py"].clone()))
                .copied()
        };
        assert_eq!(
            weight("tests/test_pricing.py"),
            Some(EDGE_WEIGHTS["test_direct"].forward)
        );
        assert_eq!(
            weight("legacy/test_pricing.py"),
            Some(EDGE_WEIGHTS["test_naming"].forward)
        );
    }
}
