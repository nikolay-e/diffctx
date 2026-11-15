//! Shell, for what the shell cell does not read: the names a script defines that
//! a pattern over its lines does not see.
//!
//! The calls, reads and writes of a script and the files it sources are the facts
//! layer's (`facts::lang::shell`, `resolve::shell`).

use std::path::Path;

use tree_sitter::Node;

#[derive(Default)]
struct ShellFacts {
    functions: Vec<String>,
}

fn read(text: &str) -> Option<ShellFacts> {
    let tree = crate::parsers::parse_tree("bash", text)?;
    let src = text.as_bytes();
    let text_of = |n: Node| n.utf8_text(src).unwrap_or("");
    let mut facts = ShellFacts::default();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
        match node.kind() {
            "function_definition" => {
                if let Some(name) = node.child_by_field_name("name") {
                    facts.functions.push(text_of(name).to_string());
                }
            }
            _ => {}
        }
    }
    Some(facts)
}

/// Whether a path is a script by its name; `None` when only its shebang
/// can say (`bin/deploy`).
fn by_path(path: &str) -> Option<bool> {
    match crate::languages::get_language_for_file(path) {
        Some(lang) => Some(matches!(lang, "bash" | "zsh")),
        None if Path::new(path).extension().is_none() => None,
        None => Some(false),
    }
}

fn by_shebang(path: &str, text: &str) -> bool {
    crate::languages::sniff_language(path, text) == Some("bash")
}

/// Whether `path`, whose text is `text`, is a script.
fn is_script(path: &str, text: &str) -> bool {
    by_path(path).unwrap_or_else(|| by_shebang(path, text))
}

/// The functions a script defines, when `path` is one.
pub(super) fn function_names(path: &str, text: &str) -> Vec<String> {
    if !is_script(path, text) {
        return Vec::new();
    }
    read(text).map_or_else(Vec::new, |f| f.functions)
}
