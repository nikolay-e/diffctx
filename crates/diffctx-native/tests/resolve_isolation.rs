//! The resolver reads a repository only through the snapshot module and the facts
//! layer: a path into any other module of the crate, or out of `resolve` through
//! `super::super`, makes the layering a convention instead of a fact.

use std::fs;
use std::path::Path;

use regex::Regex;
use walkdir::WalkDir;

/// What reaches out of `resolve`; a file may climb `reach` modules with `super`.
fn violations(source: &str, reach: usize) -> Vec<String> {
    let paths = Regex::new(r"\b(?:crate|_diffctx|diffctx)\s*::\s*(\w*)").expect("a pattern");
    let mut found: Vec<String> = paths
        .captures_iter(source)
        .filter(|hit| {
            hit.get(1)
                .is_none_or(|m| !matches!(m.as_str(), "snapshot" | "facts" | "resolve"))
        })
        .map(|hit| hit[0].to_string())
        .collect();
    let supers = Regex::new(r"(?:\bsuper\s*::\s*)+").expect("a pattern");
    for hit in supers.find_iter(source) {
        let climbed = hit.as_str().matches("super").count();
        if climbed > reach {
            found.push(hit.as_str().trim().to_string());
        }
    }
    found
}

#[test]
fn the_resolver_uses_only_the_snapshot_and_facts_modules() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/resolve");
    let mut files = 0;
    let mut reaches = Vec::new();
    for entry in WalkDir::new(&root) {
        let entry = entry.expect("a directory entry");
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let relative = path.strip_prefix(&root).expect("under the root");
        let depth = relative.components().count() - 1;
        let name = relative.to_string_lossy();
        // `super` of a module file is its parent; of `mod.rs`, the parent's parent.
        let reach = if path.file_name().is_some_and(|n| n == "mod.rs") {
            depth
        } else {
            depth + 1
        };
        files += 1;
        let text = fs::read_to_string(path).expect("a source file");
        for hit in violations(&text, reach) {
            reaches.push(format!("{name}: {hit}"));
        }
    }
    assert!(files >= 4, "the walk found {files} files under {root:?}");
    assert!(
        reaches.is_empty(),
        "resolve reaches out of itself: {reaches:#?}"
    );
}

#[test]
fn a_path_into_a_sibling_module_is_a_violation() {
    assert_eq!(
        violations("use crate::parsers::parse_tree;", 1),
        ["crate::parsers"]
    );
    assert_eq!(violations("use crate::{git, snapshot};", 1), ["crate::"]);
    assert_eq!(violations("let x = super::super::Thing;", 1).len(), 1);
    assert_eq!(violations("let x = super::super::Thing;", 2).len(), 0);
    assert!(violations("use crate::snapshot::Oid; use crate::facts::Def;", 1).is_empty());
    assert!(violations("use super::index::Index;", 1).is_empty());
    assert_eq!(violations("use super::Binding;", 0).len(), 1);
}
