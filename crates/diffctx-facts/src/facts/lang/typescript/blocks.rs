//! A block binds its `const` and `let` in itself: a read elsewhere must not
//! resolve to one, so only what a block defines by its own name is collected.

use super::{field, function_value, kids};
use tree_sitter::Node;

pub(super) fn statements(block: Node<'_>) -> Vec<Node<'_>> {
    let binds_value = |declarator: Node<'_>| {
        declarator.kind() == "variable_declarator"
            && field(declarator, "value").is_some_and(|value| function_value(value).is_none())
    };
    (kids(block).into_iter())
        .filter(|statement| !kids(*statement).into_iter().any(binds_value))
        .collect()
}
