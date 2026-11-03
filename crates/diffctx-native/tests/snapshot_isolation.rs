//! The snapshot module is the bottom of the crate: it reads a repository and
//! knows nothing of the engine above it. A path into any other module of the
//! crate, or out through `super::super`, makes the layering a convention
//! instead of a fact.

use std::fs;
use std::path::Path;

use regex::Regex;
use walkdir::WalkDir;

fn violations(source: &str) -> Vec<String> {
    let reaches_out = Regex::new(r"\b(?:crate|_diffctx|diffctx)\s*::\s*(\w*)|super\s*::\s*super\b")
        .expect("a pattern");
    reaches_out
        .captures_iter(source)
        .filter(|hit| hit.get(1).is_none_or(|m| m.as_str() != "snapshot"))
        .map(|hit| hit[0].to_string())
        .collect()
}

#[test]
fn the_snapshot_module_uses_no_other_module_of_the_crate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/snapshot");
    let mut files = 0;
    let mut reaches = Vec::new();
    for entry in WalkDir::new(&root) {
        let entry = entry.expect("a directory entry");
        if entry.path().extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        files += 1;
        let text = fs::read_to_string(entry.path()).expect("a source file");
        for hit in violations(&text) {
            reaches.push(format!("{}: {hit}", entry.path().display()));
        }
    }
    assert!(files > 10, "the walk found {files} files under {root:?}");
    assert!(
        reaches.is_empty(),
        "snapshot reaches out of itself: {reaches:#?}"
    );
}

#[test]
fn a_path_into_a_sibling_module_is_a_violation() {
    assert_eq!(violations("use crate::git::Exec;"), ["crate::git"]);
    assert_eq!(violations("use crate::{git, snapshot};"), ["crate::"]);
    assert_eq!(violations("let x = super::super::Thing;"), ["super::super"]);
    assert!(violations("use crate::snapshot::types::Oid;").is_empty());
    assert!(violations("use super::BoxError;").is_empty());
}
