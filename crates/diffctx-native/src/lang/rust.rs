//! Rust facts read off the syntax where the lexical model would guess:
//! what a name followed by `!`, or a path before it, names, which
//! functions are tests, and what a module re-exports.

use std::path::Path;

use tree_sitter::Node;

pub fn is_rust(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("rs"))
}

/// `name!(…)`: the text after a name invokes a macro, never a function.
pub fn invokes_macro(after_name: &str) -> bool {
    let rest = after_name.trim_start();
    rest.starts_with('!') && !rest.starts_with("!=")
}

/// The module a path names right before a name: `other` in `other::name`,
/// nothing in `<T as Trait>::name`.
pub fn path_qualifier(before: &str) -> Option<&str> {
    let head = before.strip_suffix("::")?;
    let start = head
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map_or(0, |i| i + 1);
    Some(&head[start..])
}

/// Whether a path qualified by `q` can reach a function of the module file
/// named `stem`: that module or an alias of it, a relative root, or any path
/// to a crate root or a `mod.rs`, whose names the stem does not give.
/// `other::name` reaches another module's `name`, never this one's.
pub fn qualifier_reaches(q: &str, stem: &str, aliases: &[String]) -> bool {
    matches!(q, "self" | "super" | "crate")
        || matches!(stem, "lib" | "main" | "mod")
        || q == stem
        || aliases.iter().any(|a| a == q)
}

/// The names a module file re-exports at its top level: `pub use a::b;`
/// binds `b`, `pub use a::{b as c, d};` binds `c` and `d`. A glob names
/// nothing it can be held to.
pub fn reexported(text: &str) -> Vec<String> {
    let Some(tree) = crate::parsers::parse_tree("rust", text) else {
        return Vec::new();
    };
    let bytes = text.as_bytes();
    let root = tree.root_node();
    let mut cursor = root.walk();
    let mut names = Vec::new();
    let mut trees: Vec<Node> = root
        .named_children(&mut cursor)
        .filter(|n| {
            n.kind() == "use_declaration"
                && n.named_child(0)
                    .is_some_and(|v| v.kind() == "visibility_modifier")
        })
        .filter_map(|n| n.child_by_field_name("argument"))
        .collect();
    while let Some(t) = trees.pop() {
        let bound = match t.kind() {
            "identifier" => Some(t),
            "scoped_identifier" => t.child_by_field_name("name"),
            "use_as_clause" => t.child_by_field_name("alias"),
            "scoped_use_list" => {
                trees.extend(t.child_by_field_name("list"));
                None
            }
            "use_list" => {
                let mut c = t.walk();
                trees.extend(t.named_children(&mut c));
                None
            }
            _ => None,
        };
        names.extend(
            bound
                .and_then(|n| n.utf8_text(bytes).ok())
                .map(str::to_string),
        );
    }
    names
}

/// The lines of the functions in `text` an attribute marks as tests:
/// `#[test]`, `#[tokio::test]` — an inline unit test is a test like any in
/// `tests/` (#528).
pub fn test_functions(text: &str) -> Vec<(u32, u32)> {
    let Some(tree) = crate::parsers::parse_tree("rust", text) else {
        return Vec::new();
    };
    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "function_item" if marked_test(child, bytes) => {
                    let row = |p: tree_sitter::Point| p.row as u32 + 1;
                    lines.push((row(child.start_position()), row(child.end_position())));
                }
                "mod_item" | "declaration_list" | "impl_item" => stack.push(child),
                _ => {}
            }
        }
    }
    lines
}

fn marked_test(func: Node, bytes: &[u8]) -> bool {
    let mut prev = func.prev_named_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" => {
                let path = p
                    .named_child(0)
                    .and_then(|a| a.named_child(0))
                    .and_then(|n| n.utf8_text(bytes).ok())
                    .unwrap_or("");
                if path.rsplit("::").next() == Some("test") {
                    return true;
                }
            }
            "line_comment" | "block_comment" => {}
            _ => return false,
        }
        prev = p.prev_named_sibling();
    }
    false
}
