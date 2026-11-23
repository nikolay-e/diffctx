//! Which functions of a Rust file an attribute or a macro marks as tests, off the
//! Rust grammar: the impact answer's reach into inline unit tests (#528).

use std::hash::{Hash, Hasher};
use std::str::Utf8Error;
use std::sync::{Arc, Mutex, MutexGuard};

use once_cell::sync::Lazy;
use rustc_hash::{FxHashMap, FxHasher};
use tree_sitter::{Node, Tree};

const CACHE_LIMIT: usize = 8192;
type Key = (u64, usize);

static CACHE: Lazy<Mutex<FxHashMap<Key, Arc<[(u32, u32)]>>>> =
    Lazy::new(|| Mutex::new(FxHashMap::default()));

fn key(text: &str) -> Key {
    let mut h = FxHasher::default();
    text.hash(&mut h);
    (h.finish(), text.len())
}

fn lock_cache() -> MutexGuard<'static, FxHashMap<Key, Arc<[(u32, u32)]>>> {
    CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn remember(key: Key, spans: Arc<[(u32, u32)]>) {
    let mut cache = lock_cache();
    if cache.len() >= CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(key, spans);
}

fn may_hold_tests(text: &str) -> bool {
    diffctx_base::testfiles::RUST_TEST_ATTRIBUTES
        .iter()
        .chain(diffctx_base::testfiles::RUST_TEST_MACROS)
        .any(|marker| text.contains(marker))
}

pub(super) fn record(tree: &Tree, text: &str) {
    let key = key(text);
    let known = lock_cache().contains_key(&key);
    if known {
        return;
    }
    if !may_hold_tests(text) {
        remember(key, Arc::from(&[][..]));
        return;
    }
    if let Ok(lines) = extract(tree.root_node(), text.as_bytes()) {
        remember(key, lines.into());
    }
}

/// The lines of the functions in `text` an attribute marks as tests (`#[test]`,
/// `#[tokio::test]`, `#[rstest]`, `#[test_case(..)]`), and of the macro blocks
/// that make tests of the functions inside them (`proptest! { .. }`) — an inline
/// unit test is a test like any in `tests/` (#528). The attributes and macros are
/// the rows of `testfiles::RUST_TEST_ATTRIBUTES` and `RUST_TEST_MACROS`.
pub fn test_functions(text: &str) -> Result<Vec<(u32, u32)>, Utf8Error> {
    let key = key(text);
    let hit = lock_cache().get(&key).cloned();
    if let Some(spans) = hit {
        return Ok(spans.to_vec());
    }
    if !may_hold_tests(text) {
        remember(key, Arc::from(&[][..]));
        return Ok(Vec::new());
    }
    let Some(tree) = crate::parsers::parse_tree("rust", text) else {
        return Ok(Vec::new());
    };
    let lines = extract(tree.root_node(), text.as_bytes())?;
    remember(key, lines.clone().into());
    Ok(lines)
}

fn extract(root: Node, bytes: &[u8]) -> Result<Vec<(u32, u32)>, Utf8Error> {
    let row = |p: tree_sitter::Point| p.row as u32 + 1;
    let mut lines = Vec::new();
    let mut stack = vec![root];
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
