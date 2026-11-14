//! What the impact answer still reads of Rust off its text: whether a file is
//! Rust, whether every module on its path is public, and which functions an
//! attribute marks as tests. Callers, definitions and removals are the facts
//! layer's (`facts::lang::rust`, `resolve::rust`), answered through `decide`.

use std::path::Path;
use std::str::Utf8Error;

use tree_sitter::Node;

use crate::source::Source;

pub(super) fn is_rust(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("rs"))
}

/// Whether every module on a Rust file's path is public from its crate
/// root: `pub fn` in a `pub(crate) mod` is crate-internal (#359). A binary
/// crate exports nothing. Read off the snapshot, never the disk (an
/// uncommitted `mod.rs` -> `m.rs` move flipped a commit's verdict).
pub(super) fn module_is_public(source: &Source, path: &str) -> bool {
    if !is_rust(path) {
        return true;
    }
    let read = |p: &Path| source.read_to_string(p);
    let file = Path::new(path);
    let Some(src_dir) = file.ancestors().find(|d| {
        d.file_name().is_some_and(|n| n == "src")
            && d.parent()
                .is_some_and(|c| read(&c.join("Cargo.toml")).is_some())
    }) else {
        return true;
    };
    let Ok(rel_path) = file.strip_prefix(src_dir) else {
        return true;
    };
    let mut segments: Vec<String> = rel_path
        .with_extension("")
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    if segments.last().is_some_and(|s| s == "mod") {
        segments.pop();
    }
    if segments.first().is_some_and(|s| s == "main" || s == "bin") {
        return false;
    }
    if segments == ["lib"] {
        return true;
    }
    let mut parent = src_dir.join("lib.rs");
    for (i, seg) in segments.iter().enumerate() {
        let Some(text) = read(&parent) else {
            return i > 0;
        };
        let declared_pub = text.lines().map(without_attributes).any(|l| {
            l.strip_prefix("pub mod ")
                .and_then(|rest| {
                    rest.trim_start()
                        .trim_start_matches("r#")
                        .strip_prefix(seg.as_str())
                })
                .is_some_and(|after| after.trim_start().starts_with([';', '{']))
                || (l.starts_with("pub use ") && l.contains(&format!("{seg}::")))
        });
        if !declared_pub {
            return false;
        }
        let dir = parent.parent().unwrap_or(src_dir).to_path_buf();
        let base = if parent
            .file_name()
            .is_some_and(|n| n == "lib.rs" || n == "mod.rs")
        {
            dir
        } else {
            parent
                .file_stem()
                .map_or(dir.clone(), |stem| dir.join(stem))
        };
        let flat = base.join(format!("{seg}.rs"));
        parent = if read(&flat).is_some() {
            flat
        } else {
            base.join(seg).join("mod.rs")
        };
    }
    true
}

fn without_attributes(line: &str) -> &str {
    let mut rest = line.trim_start();
    while let Some(attr) = rest.strip_prefix("#[") {
        let mut depth = 1usize;
        let Some(end) = attr.char_indices().find_map(|(i, c)| {
            match c {
                '[' => depth += 1,
                ']' => depth -= 1,
                _ => {}
            }
            (depth == 0).then_some(i)
        }) else {
            return rest;
        };
        rest = attr[end + 1..].trim_start();
    }
    rest
}

/// The lines of the functions in `text` an attribute marks as tests (`#[test]`,
/// `#[tokio::test]`, `#[rstest]`, `#[test_case(..)]`), and of the macro blocks
/// that make tests of the functions inside them (`proptest! { .. }`) — an inline
/// unit test is a test like any in `tests/` (#528). The attributes and macros are
/// the rows of `testfiles::RUST_TEST_ATTRIBUTES` and `RUST_TEST_MACROS`.
pub(super) fn test_functions(text: &str) -> Result<Vec<(u32, u32)>, Utf8Error> {
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
    Ok(crate::testfiles::RUST_TEST_MACROS.contains(&last_segment(name, bytes)?))
}

fn marked_test(func: Node, bytes: &[u8]) -> Result<bool, Utf8Error> {
    let mut prev = func.prev_named_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" => {
                if let Some(path) = p.named_child(0).and_then(|a| a.named_child(0)) {
                    if crate::testfiles::RUST_TEST_ATTRIBUTES.contains(&last_segment(path, bytes)?)
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
