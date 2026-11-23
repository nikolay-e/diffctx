use std::sync::{Arc, Mutex};

use once_cell::sync::Lazy;
use rustc_hash::FxHashMap;
use tree_sitter::{Language, Node, Tree};

use diffctx_config::config::parsers::PARSERS;

use super::tree_sitter_strategy::node_type_to_kind;

pub(super) type KindTable = Arc<[Option<&'static str>]>;

static TABLES: Lazy<Mutex<FxHashMap<&'static str, KindTable>>> =
    Lazy::new(|| Mutex::new(FxHashMap::default()));

pub(super) fn for_language(
    ts_name: &'static str,
    language: &Language,
    definition_types: &[&str],
) -> KindTable {
    let known = TABLES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(ts_name)
        .cloned();
    if let Some(table) = known {
        return table;
    }
    let count = language.node_kind_count();
    let table: KindTable = (0..count)
        .map(|id| {
            let name = u16::try_from(id)
                .ok()
                .and_then(|id| language.node_kind_for_id(id))?;
            definition_types
                .contains(&name)
                .then(|| node_type_to_kind(name, None))
        })
        .collect::<Vec<_>>()
        .into();
    let mut map = TABLES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(map.entry(ts_name).or_insert(table))
}

pub(super) fn record_parsed(ts_name: &str, tree: &Tree, content: &str) {
    match ts_name {
        "kotlin" => {
            crate::facts::kotlin::record(tree, content);
        }
        "rust" => super::rust_tests::record(tree, content),
        "python" => super::python_decls::record(tree, content),
        _ => {}
    }
}

pub(super) fn adjust_start_for_ancestor(
    parent: Option<Node>,
    grandparent: Option<Node>,
    start: u32,
) -> u32 {
    let is_wrapper = |a: &Node| matches!(a.kind(), "export_statement" | "decorated_definition");
    let ancestor = match parent {
        Some(a) if is_wrapper(&a) => Some(a),
        Some(_) => grandparent.filter(is_wrapper),
        None => None,
    };
    match ancestor {
        Some(a) => (a.start_position().row as u32 + 1).min(start),
        None => start,
    }
}

pub(super) fn is_loop_counter(
    node: &Node,
    parent: Option<Node>,
    grandparent: Option<Node>,
) -> bool {
    if node.kind() != "variable_declarator" {
        return false;
    }
    let (Some(declaration), Some(stmt)) = (parent, grandparent) else {
        return false;
    };
    stmt.kind() == "for_statement"
        && stmt
            .child_by_field_name("initializer")
            .is_some_and(|init| init.id() == declaration.id())
}

pub(super) fn first_child_def_line(
    node: &Node,
    def_kinds: &[Option<&'static str>],
    depth: u32,
) -> Option<u32> {
    if depth > PARSERS.container_search_max_depth {
        return None;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if def_kinds
            .get(usize::from(child.kind_id()))
            .copied()
            .flatten()
            .is_some()
        {
            return Some(child.start_position().row as u32 + 1);
        }
        if let Some(line) = first_child_def_line(&child, def_kinds, depth + 1) {
            return Some(line);
        }
    }
    None
}
