//! Scala facts from `tree-sitter-scala`: `package_clause` chains,
//! `import_declaration` selectors (braces, `=>` and `as` renames, `_`/`*`/
//! `given` wildcards), `extends_clause` types.

use rustc_hash::FxHashSet;
use tree_sitter::Node;

use super::{ImportFact, LanguageFacts};

/// `None` when the grammar is unavailable, the parse timed out, or the text
/// does not parse cleanly — a fragment cut mid-scope, a file mid-edit. The
/// caller keeps its regex reader for exactly those inputs.
pub fn facts(content: &str) -> Option<LanguageFacts> {
    let tree = crate::parsers::parse_tree("scala", content)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }
    let bytes = content.as_bytes();
    let mut facts = LanguageFacts::default();
    let mut package_parts: Vec<String> = Vec::new();
    let mut package_open = true;
    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        match node.kind() {
            // Scala chains package clauses (`package com.acme` then
            // `package svc` is com.acme.svc); a block-form clause opens a
            // scope whose siblings are not chain segments, so the chain
            // stops at the first one.
            "package_clause" => {
                if package_open {
                    if let Some(name) = node.child_by_field_name("name") {
                        package_parts.push(text(name, bytes).to_string());
                    }
                    if node.child_by_field_name("body").is_some() {
                        package_open = false;
                    }
                }
                collect_definitions(node, bytes, &mut facts.inherits);
            }
            "import_declaration" => {
                if let Some(fact) = import_fact(node, bytes) {
                    facts.imports.push(fact);
                }
            }
            _ => collect_definitions(node, bytes, &mut facts.inherits),
        }
    }
    if !package_parts.is_empty() {
        facts.package = Some(package_parts.join("."));
    }
    Some(facts)
}

fn text<'a>(node: Node, bytes: &'a [u8]) -> &'a str {
    node.utf8_text(bytes).unwrap_or("").trim()
}

fn import_fact(node: Node, bytes: &[u8]) -> Option<ImportFact> {
    let mut path: Vec<String> = Vec::new();
    let mut selectors: Vec<String> = Vec::new();
    let mut wildcard = false;
    let mut had_selector_group = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" if child.parent().is_some_and(|p| p.id() == node.id()) => {
                path.push(text(child, bytes).to_string());
            }
            "namespace_wildcard" => wildcard = true,
            "as_renamed_identifier" | "arrow_renamed_identifier" => {
                had_selector_group = true;
                if let Some(name) = child.child_by_field_name("name") {
                    selectors.push(text(name, bytes).to_string());
                }
            }
            "namespace_selectors" => {
                had_selector_group = true;
                let mut inner = child.walk();
                for sel in child.children(&mut inner) {
                    match sel.kind() {
                        "identifier" => selectors.push(text(sel, bytes).to_string()),
                        "namespace_wildcard" => wildcard = true,
                        "as_renamed_identifier" | "arrow_renamed_identifier" => {
                            if let Some(name) = sel.child_by_field_name("name") {
                                selectors.push(text(name, bytes).to_string());
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    if path.is_empty() {
        return None;
    }
    // `import a.b.C`: the last path segment is the selector, the rest the
    // prefix — the same split the regex reader made.
    if !wildcard && !had_selector_group {
        let last = path.pop()?;
        selectors.push(last);
    }
    // A selector starting lowercase after a wildcard-less brace group is a
    // sub-package import (`import scala.collection.{mutable, immutable}`);
    // the reader keeps it, matching the regex behaviour.
    Some(ImportFact {
        prefix: path.join("."),
        selectors,
        wildcard,
    })
}

/// Every `extends_clause` type name under `node`, at any depth: `Bar` from
/// `extends Bar with Baz`, `A` from `extends A[Int]`, `C` from `a.b.C`.
fn collect_definitions(node: Node, bytes: &[u8], inherits: &mut FxHashSet<String>) {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "extends_clause" {
            let mut cursor = n.walk();
            for child in n.children(&mut cursor) {
                if let Some(name) = outer_type_name(child, bytes) {
                    inherits.insert(name);
                }
            }
            continue;
        }
        let mut cursor = n.walk();
        for child in n.children(&mut cursor) {
            stack.push(child);
        }
    }
}

fn outer_type_name(node: Node, bytes: &[u8]) -> Option<String> {
    match node.kind() {
        "type_identifier" => Some(text(node, bytes).to_string()),
        "generic_type" | "projection_type" | "compound_type" | "infix_type" => node
            .child_by_field_name("type")
            .or_else(|| node.child(0))
            .and_then(|t| outer_type_name(t, bytes)),
        "stable_type_identifier" => {
            let full = text(node, bytes);
            full.rsplit('.').next().map(str::to_string)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn imports_of(src: &str) -> Vec<ImportFact> {
        facts(src).expect("parses").imports
    }

    #[test]
    fn imports_split_prefix_selectors_and_wildcards_like_the_reader_did() {
        let got = imports_of(
            "import repo.UserRepository\nimport com.foo._\nimport http.{Request, Response}\nimport a.b.{C => D, _}\nimport a.{B as C, D}\nimport Tables._\nimport a.b.Conf as Config\n",
        );
        let want = vec![
            ImportFact {
                prefix: "repo".into(),
                selectors: vec!["UserRepository".into()],
                wildcard: false,
            },
            ImportFact {
                prefix: "com.foo".into(),
                selectors: vec![],
                wildcard: true,
            },
            ImportFact {
                prefix: "http".into(),
                selectors: vec!["Request".into(), "Response".into()],
                wildcard: false,
            },
            ImportFact {
                prefix: "a.b".into(),
                selectors: vec!["C".into()],
                wildcard: true,
            },
            ImportFact {
                prefix: "a".into(),
                selectors: vec!["B".into(), "D".into()],
                wildcard: false,
            },
            ImportFact {
                prefix: "Tables".into(),
                selectors: vec![],
                wildcard: true,
            },
            ImportFact {
                prefix: "a.b".into(),
                selectors: vec!["Conf".into()],
                wildcard: false,
            },
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn a_multiline_brace_import_and_packages_come_off_the_tree() {
        let f = facts("package com.acme\npackage service\n\nimport scala.collection.{\n  mutable,\n  immutable\n}\nimport a.B\n\npackage object strings { def slug = 1 }\nclass X extends Y[Int] with Z {}\n")
            .expect("parses");
        assert_eq!(f.package.as_deref(), Some("com.acme.service"));
        assert_eq!(
            f.imports[0].selectors,
            vec!["mutable".to_string(), "immutable".to_string()]
        );
        assert_eq!(f.imports[1].selectors, vec!["B".to_string()]);
        let mut inherits: Vec<&str> = f.inherits.iter().map(String::as_str).collect();
        inherits.sort();
        assert_eq!(inherits, vec!["Y", "Z"]);
    }

    #[test]
    fn block_packages_stop_the_chain_and_comments_hold_no_imports() {
        let f = facts(
            "package com.example\npackage util {\n  class A\n}\npackage data {\n  class B\n}\n",
        )
        .expect("parses");
        assert_eq!(f.package.as_deref(), Some("com.example.util"));
        let f = facts("/*\nimport ghost.Gone\n*/\nimport real.Thing\n").expect("parses");
        assert_eq!(f.imports.len(), 1);
        assert_eq!(f.imports[0].prefix, "real");
    }

    #[test]
    fn broken_text_yields_no_facts_so_the_reader_falls_back() {
        assert!(facts("import a.{\nclass X {").is_none());
    }
}
