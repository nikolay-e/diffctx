//! Which functions of a Rust file an attribute or a macro marks as tests, off the
//! Rust grammar: the impact answer's reach into inline unit tests (#528).

use std::str::Utf8Error;

use tree_sitter::Node;

/// The lines of the functions in `text` an attribute marks as tests (`#[test]`,
/// `#[tokio::test]`, `#[rstest]`, `#[test_case(..)]`), and of the macro blocks
/// that make tests of the functions inside them (`proptest! { .. }`) — an inline
/// unit test is a test like any in `tests/` (#528). The attributes and macros are
/// the rows of `testfiles::RUST_TEST_ATTRIBUTES` and `RUST_TEST_MACROS`.
pub fn test_functions(text: &str) -> Result<Vec<(u32, u32)>, Utf8Error> {
    let Some(tree) = crate::parsers::parse_tree("rust", text) else {
        return Ok(Vec::new());
    };
    let bytes = text.as_bytes();
    let row = |p: tree_sitter::Point| p.row as u32 + 1;
    let mut lines = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "function_item" if marked_test(child, bytes)? => {
                    lines.push((row(child.start_position()), row(child.end_position())));
                }
                "macro_invocation" if is_test_macro(child, bytes)? => {
                    lines.push((row(child.start_position()), row(child.end_position())));
                }
                "mod_item" | "declaration_list" | "impl_item" => stack.push(child),
                _ => {}
            }
        }
    }
    Ok(lines)
}

/// The last segment of `node`'s path (`tokio::test` is `test`).
fn last_segment<'a>(node: Node, bytes: &'a [u8]) -> Result<&'a str, Utf8Error> {
    let path = node.utf8_text(bytes)?;
    Ok(path.rsplit_once("::").map_or(path, |(_, last)| last))
}

fn is_test_macro(invocation: Node, bytes: &[u8]) -> Result<bool, Utf8Error> {
    let Some(name) = invocation.child_by_field_name("macro") else {
        return Ok(false);
    };
    Ok(diffctx_base::testfiles::RUST_TEST_MACROS.contains(&last_segment(name, bytes)?))
}

fn marked_test(func: Node, bytes: &[u8]) -> Result<bool, Utf8Error> {
    let mut prev = func.prev_named_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" => {
                if let Some(path) = p.named_child(0).and_then(|a| a.named_child(0)) {
                    if diffctx_base::testfiles::RUST_TEST_ATTRIBUTES
                        .contains(&last_segment(path, bytes)?)
                    {
                        return Ok(true);
                    }
                }
            }
            "line_comment" | "block_comment" => {}
            _ => return Ok(false),
        }
        prev = p.prev_named_sibling();
    }
    Ok(false)
}
