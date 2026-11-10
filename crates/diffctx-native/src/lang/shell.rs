//! Shell, for what the shell cell does not read: the names a script defines that
//! a pattern over its lines does not see.
//!
//! The calls, reads and writes of a script and the files it sources are the facts
//! layer's (`facts::lang::shell`, `resolve::shell`); this model answers no
//! relation of its own.

use std::cell::RefCell;
use std::path::Path;

use rustc_hash::FxHashMap;
use tree_sitter::Node;

use super::{At, DefId, LanguageFacts, Relation, UseSite};
use crate::bindings::Finding;
use crate::source::Source;

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

/// The functions a script defines, when `path` is one.
pub(super) fn function_names(path: &str, text: &str) -> Vec<String> {
    if !by_path(path).unwrap_or_else(|| by_shebang(path, text)) {
        return Vec::new();
    }
    read(text).map_or_else(Vec::new, |f| f.functions)
}

pub(super) struct ShellModel<'a> {
    root: &'a Path,
    source: &'a Source,
    scripts: RefCell<FxHashMap<String, bool>>,
}

impl<'a> ShellModel<'a> {
    pub(super) fn new(root: &'a Path, source: &'a Source) -> Self {
        Self {
            root,
            source,
            scripts: RefCell::default(),
        }
    }
}

impl LanguageFacts for ShellModel<'_> {
    fn modelled(&self) -> &'static [Relation] {
        &[]
    }

    fn covers(&self, path: &str) -> bool {
        if let Some(known) = by_path(path) {
            return known;
        }
        if let Some(known) = self.scripts.borrow().get(path) {
            return *known;
        }
        let script = self
            .source
            .read_to_string(&self.root.join(path))
            .is_some_and(|text| by_shebang(path, &text));
        self.scripts.borrow_mut().insert(path.to_string(), script);
        script
    }

    fn definition(&self, _at: &At) -> Option<DefId> {
        None
    }

    fn classify(&self, _site: &UseSite, _def: DefId) -> Option<Finding> {
        None
    }

    fn reaches_file(&self, _test: &str, _caller: &str) -> bool {
        false
    }
}
