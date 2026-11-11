//! The facts layer reads a repository only through `crate::snapshot`: a path into
//! any other module of the crate, or out of `facts` through `super::super`, makes
//! the layering a convention instead of a fact. The four files of the first
//! layer (`facts::java`, `kotlin`, `python`, `scala`) are the syntax
//! facts the edge builders read today and parse through the facade; they are the
//! layer this one replaces and are not held to it.

use std::fs;
use std::path::Path;

use regex::Regex;
use walkdir::WalkDir;

mod isolation_source;
use isolation_source::without_comments;

const FIRST_LAYER: [&str; 4] = ["java.rs", "kotlin.rs", "python.rs", "scala.rs"];

/// What reaches out of `facts`; a file may climb `reach` modules with `super`.
fn violations(source: &str, reach: usize) -> Vec<String> {
    let source = without_comments(source);
    let source = source.as_str();
    let paths = Regex::new(r"\b(?:crate|_diffctx|diffctx)\s*::\s*(\w*)").expect("a pattern");
    let mut found: Vec<String> = paths
        .captures_iter(source)
        .filter(|hit| {
            hit.get(1)
                .is_none_or(|m| !matches!(m.as_str(), "snapshot" | "facts"))
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
fn the_facts_layer_uses_only_the_snapshot_module() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/facts");
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
        if depth == 0 && FIRST_LAYER.contains(&name.as_ref()) {
            continue;
        }
        files += 1;
        let text = fs::read_to_string(path).expect("a source file");
        for hit in violations(&text, reach) {
            reaches.push(format!("{name}: {hit}"));
        }
    }
    assert!(files >= 10, "the walk found {files} files under {root:?}");
    assert!(
        reaches.is_empty(),
        "facts reaches out of itself: {reaches:#?}"
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
    assert_eq!(
        violations("let x = super::super::super::Thing;", 2).len(),
        1
    );
    assert!(violations("use crate::snapshot::Oid; use crate::facts::Def;", 1).is_empty());
    assert!(violations("use super::model::Def;", 1).is_empty());
    assert_eq!(violations("use super::Def;", 0).len(), 1);
    // A comment that names a path outside the module reaches nothing; the code
    // around it, and a string that holds `//`, still count.
    assert!(violations("// use crate::parsers::x; super::super::y\n/// crate::a\n//! super::super\n/* crate::b /* super::super */ crate::c */ fn f() {}", 1).is_empty());
    assert_eq!(
        violations("fn f() {} /* a note */ use crate::parsers::Z;", 1).len(),
        1
    );
    assert_eq!(
        violations("let url = \"http://x\"; use crate::parsers::Z;", 1).len(),
        1
    );
    assert_eq!(
        violations("let raw = r#\"//\"#; use crate::parsers::Z;", 1).len(),
        1
    );
}
