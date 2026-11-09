//! Fields that look like references and no row names: a key that ends in `Name`
//! or `Ref` with a name for its value (`storageClassName: fast`), or a `*Ref` map
//! with a `name` (`credentialsRef: {name: db}`).
//!
//! They are candidates and only candidates: a suffix says nothing of what kind of
//! object is meant, and most such keys name no object at all (`nodeName`,
//! `containerName`). One matters only where an object of that name stands in the
//! repository, which is the resolver's question.

use std::ops::Range;

use super::tree::{Node, Value};

/// A name a candidate field holds.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Candidate<'a> {
    /// The key that holds it.
    pub field: &'a str,
    pub name: &'a Node,
}

/// Nesting followed below an object.
const MAX_DEPTH: usize = 24;

/// The candidates of an object's node, in source order, leaving out the fields
/// at `typed` (the sites of references a row already named).
pub fn candidates<'a>(object: &'a Node, typed: &[Range<u32>]) -> Vec<Candidate<'a>> {
    let mut found = Vec::new();
    walk(object, typed, 0, &mut found);
    found
}

fn walk<'a>(node: &'a Node, typed: &[Range<u32>], depth: usize, found: &mut Vec<Candidate<'a>>) {
    if depth > MAX_DEPTH {
        return;
    }
    match &node.value {
        Value::Map(entries) => {
            for entry in entries {
                let key = entry.key.text.as_str();
                let name = if let Some(text) = entry.value.as_str() {
                    key.ends_with("Name")
                        .then_some(&entry.value)
                        .filter(|_| is_name(text))
                } else if key.ends_with("Ref") {
                    entry
                        .value
                        .get("name")
                        .filter(|name| name.as_str().is_some_and(is_name))
                } else {
                    None
                };
                match name {
                    Some(name) if !typed.contains(&name.span.bytes) && !name.templated => {
                        found.push(Candidate { field: key, name });
                    }
                    _ => walk(&entry.value, typed, depth + 1, found),
                }
            }
        }
        Value::Seq(items) => {
            for item in items {
                walk(item, typed, depth + 1, found);
            }
        }
        Value::Null | Value::Scalar(_) => {}
    }
}

/// What a name of an object can be: a DNS-style label or subdomain, with the
/// colons of a cluster role's name. Not a sentence, a path or a template.
fn is_name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 253
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':' | '_'))
}
