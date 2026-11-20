// What the structure tests share: the Rust grammar over a source file, and the
// caps `budget.toml` holds for them.

use std::fs;
use std::path::{Path, PathBuf};

use tree_sitter::{Node, Parser};

pub(crate) struct Source<'a> {
    pub(crate) text: &'a str,
}

impl Source<'_> {
    pub(crate) fn of(&self, node: Node) -> &str {
        &self.text[node.byte_range()]
    }
}

pub(crate) fn parse(path: &Path) -> (String, tree_sitter::Tree) {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .expect("rust grammar");
    let tree = parser.parse(&text, None).expect("parse");
    (text, tree)
}

pub(crate) fn budget_number(budget: &toml::Table, key: &str) -> usize {
    budget
        .get(key)
        .and_then(toml::Value::as_integer)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or_else(|| panic!("budget.toml: {key} is missing or not a count"))
}

pub(crate) fn ratchet(problems: &mut Vec<String>, key: &str, recorded: usize, measured: usize) {
    if measured > recorded {
        problems.push(format!("{key} grew {recorded} -> {measured}"));
    } else if measured < recorded {
        problems.push(format!(
            "{key} dropped {recorded} -> {measured}: lower it in budget.toml in the same commit"
        ));
    }
}

pub(crate) fn crate_file(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}
