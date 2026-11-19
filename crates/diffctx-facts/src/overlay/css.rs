//! CSS, for the fragments of a stylesheet: a rule is named by the class it styles,
//! which a rule fragment carries no name for. Who spells the class in markup is
//! read by no cell: the answer says so.

use diffctx_base::types::FragmentKind;
use tree_sitter::Node;

pub(super) fn is_stylesheet(path: &str) -> bool {
    matches!(
        diffctx_base::languages::get_language_for_file(path),
        Some("css")
    )
}

/// The rules of `text` as `(class, lines, kind)`: each is named by the class its
/// first selector ends on (`.nav .btn` styles `btn`); a rule with no class selector
/// has no name. A class listed after the first selector of a rule is not named.
pub(super) fn declarations(text: &str) -> Vec<(Option<String>, (u32, u32), FragmentKind)> {
    let Some(tree) = crate::parsers::parse_tree("css", text) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
        if node.kind() != "rule_set" {
            continue;
        }
        if let Some(class) = styled_class(node, text) {
            found.push((
                Some(class),
                (
                    node.start_position().row as u32 + 1,
                    node.end_position().row as u32 + 1,
                ),
                FragmentKind::Definition,
            ));
        }
    }
    found
}

fn styled_class(rule: Node, text: &str) -> Option<String> {
    let mut cursor = rule.walk();
    let selectors = rule
        .children(&mut cursor)
        .find(|c| c.kind() == "selectors")?;
    let first = selectors.named_child(0)?;
    let mut last = None;
    let mut stack = vec![first];
    while let Some(node) = stack.pop() {
        if node.kind() == "class_name" {
            if last.is_none_or(|l: Node| l.start_byte() < node.start_byte()) {
                last = Some(node);
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    last.map(|n| text[n.byte_range()].to_string())
}
