//! JSON (and the `//` and `/* */` comments the grammar accepts) read with the
//! `tree-sitter-json` grammar. Several top-level values are several documents.

use tree_sitter::{Language as TsLanguage, Node as TsNode};

use crate::snapshot::RunBudget;

use super::syntax::{self, Guard};
use super::tree::{Entry, Key, Node, Scalar, ScalarKind, Value};
use super::{Flaw, Reading};

#[cfg(feature = "tree-sitter-json")]
fn grammar() -> Option<TsLanguage> {
    Some(TsLanguage::new(tree_sitter_json::LANGUAGE))
}

#[cfg(not(feature = "tree-sitter-json"))]
fn grammar() -> Option<TsLanguage> {
    None
}

pub(super) fn read(text: &str, budget: &RunBudget, out: &mut Reading) {
    let Some(grammar) = grammar() else {
        out.flaws.push(Flaw::Unsupported);
        return;
    };
    let Some(tree) = syntax::parse(&grammar, text.as_bytes(), budget) else {
        out.flaws.push(if crate::facts::expired(budget) {
            Flaw::Deadline
        } else {
            Flaw::Unsupported
        });
        return;
    };
    let root = tree.root_node();
    if root.has_error() {
        out.errors = syntax::error_lines(root);
    }
    let mut reader = Reader {
        text,
        guard: Guard::new(budget),
    };
    out.documents = syntax::children(root)
        .into_iter()
        .map(|value| reader.value(value, 0))
        .collect();
    out.flaws.extend(reader.guard.flaws);
}

struct Reader<'a> {
    text: &'a str,
    guard: Guard<'a>,
}

impl Reader<'_> {
    fn slice(&self, node: TsNode) -> &str {
        self.text
            .get(node.start_byte()..node.end_byte())
            .unwrap_or("")
    }

    fn scalar(&self, at: TsNode, text: String, kind: ScalarKind, quoted: bool) -> Node {
        Node {
            span: syntax::span(at),
            value: Value::Scalar(Scalar { text, kind, quoted }),
            templated: false,
        }
    }

    fn value(&mut self, at: TsNode, depth: usize) -> Node {
        if !self.guard.enter(depth) {
            return Node::null(syntax::span(at));
        }
        match at.kind() {
            "object" => {
                let mut entries = Vec::new();
                for pair in syntax::children(at) {
                    if pair.kind() != "pair" {
                        continue;
                    }
                    let Some(key) = pair.child_by_field_name("key") else {
                        continue;
                    };
                    let value = match pair.child_by_field_name("value") {
                        Some(value) => self.value(value, depth + 1),
                        None => Node::null(syntax::span(pair)),
                    };
                    entries.push(Entry {
                        key: Key {
                            text: self.string(key),
                            span: syntax::span(key),
                            templated: false,
                        },
                        value,
                    });
                }
                Node {
                    span: syntax::span(at),
                    value: Value::Map(entries),
                    templated: false,
                }
            }
            "array" => {
                let items = syntax::children(at)
                    .into_iter()
                    .map(|item| self.value(item, depth + 1))
                    .collect();
                Node {
                    span: syntax::span(at),
                    value: Value::Seq(items),
                    templated: false,
                }
            }
            "string" => {
                let text = self.string(at);
                self.scalar(at, text, ScalarKind::Str, true)
            }
            "number" => {
                let text = self.slice(at).to_string();
                let float = text.contains(['.', 'e', 'E']);
                let kind = if float {
                    ScalarKind::Float
                } else {
                    ScalarKind::Int
                };
                self.scalar(at, text, kind, false)
            }
            "true" | "false" => {
                let text = self.slice(at).to_string();
                self.scalar(at, text, ScalarKind::Bool, false)
            }
            _ => Node::null(syntax::span(at)),
        }
    }

    /// A string with its escapes decoded; one the escapes of JSON do not read
    /// (a raw control character, a bad `\u`) is its text between the quotes.
    fn string(&self, at: TsNode) -> String {
        let raw = self.slice(at);
        match serde_json::from_str::<String>(raw) {
            Ok(text) => text,
            Err(_) => raw
                .get(1..raw.len().saturating_sub(1))
                .unwrap_or(raw)
                .to_string(),
        }
    }
}
