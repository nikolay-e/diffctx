//! Label selectors and the labels they select by.
//!
//! A Service, a PodDisruptionBudget, a NetworkPolicy or a workload's own
//! `matchLabels` names no object: it names the labels of the pods (or objects)
//! it selects. Its reach is every object in scope whose labels hold all of its
//! pairs, which is a candidate by nature: the selector may also carry
//! expressions, the labels may be set by a template or an operator, and a pod
//! that carries them may not exist. So a selector is a fact of its own, matched
//! against the label facts of the other objects by the resolver, and never
//! resolved.
//!
//! The labels of an object are definitions of their own (`labels`, and for a
//! workload `pod-labels`, the labels of its pod template), so an edit of them is
//! an edit of that definition and its selectors are its readers; any other edit
//! of the object reaches the selectors not at all.

use serde::{Deserialize, Serialize};

use crate::facts::{Scope, Span};

use super::object::Found;
use super::tree::{FieldPath, Node, Value};

/// Which labels a selector selects by.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum LabelSet {
    /// The labels of a pod: a workload's pod template (`pod-labels`), a `Pod`'s
    /// own.
    Pod,
    /// The labels an object carries in `metadata.labels`.
    Own,
}

/// A selector of an object: the pairs it requires, and what it selects.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Selector {
    /// The map of pairs in the file.
    pub site: Span,
    /// The definition of the object that holds the selector.
    pub enclosing: Scope,
    /// The path of the selector, as the table writes it.
    pub field: String,
    pub matches: LabelSet,
    /// The kinds of the objects it can select; any kind when empty.
    pub target_kinds: Vec<String>,
    /// The namespace the selector's own object stands in; a selector reaches
    /// that namespace alone.
    pub namespace: Option<String>,
    pub pairs: Vec<(String, String)>,
    /// The `matchExpressions` of a label selector, all of which must hold too.
    pub expressions: Vec<Expression>,
}

/// How an expression tests the value of a label key.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Operator {
    In,
    NotIn,
    Exists,
    DoesNotExist,
}

/// One entry of a selector's `matchExpressions`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Expression {
    pub key: String,
    pub operator: Operator,
    pub values: Vec<String>,
}

impl Selector {
    /// Whether an object that carries `labels` is selected: every pair is among
    /// them and every expression holds.
    pub fn selects(&self, labels: &[(String, String)]) -> bool {
        self.pairs.iter().all(|pair| labels.contains(pair))
            && self.expressions.iter().all(|expression| {
                let held = labels.iter().find(|(key, _)| *key == expression.key);
                match expression.operator {
                    Operator::In => held.is_some_and(|(_, v)| expression.values.contains(v)),
                    Operator::NotIn => held.is_none_or(|(_, v)| !expression.values.contains(v)),
                    Operator::Exists => held.is_some(),
                    Operator::DoesNotExist => held.is_none(),
                }
            })
    }
}

/// The labels of one label definition: its pairs, as written.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Labels {
    pub site: Span,
    /// The label definition (`labels` or `pod-labels` of an object).
    pub enclosing: Scope,
    pub pairs: Vec<(String, String)>,
}

/// One row: in an object of one of `kinds`, the map at `path` selects.
#[derive(Clone, Copy, Debug)]
pub struct SelectorRow {
    pub kinds: &'static [&'static str],
    pub path: &'static str,
    /// The node at `path` is a label selector (`matchLabels` and
    /// `matchExpressions`), not a map of pairs.
    pub label_selector: bool,
    pub matches: LabelSet,
    pub target_kinds: &'static [&'static str],
}

/// The string pairs of a map node, in source order; a value that is not a plain
/// string (an expression list, a number) or that a template wrote is no pair.
pub fn pairs_of(node: &Node) -> Vec<(String, String)> {
    let Value::Map(entries) = &node.value else {
        return Vec::new();
    };
    entries
        .iter()
        .filter(|entry| !entry.key.templated && !entry.value.templated)
        .filter_map(|entry| {
            let value = entry.value.as_str()?;
            Some((entry.key.text.clone(), value.to_string()))
        })
        .collect()
}

/// The expressions of a `matchExpressions` list, in source order; an entry that
/// states no known operator or key is no expression, so the selector reads wider
/// than it is.
fn expressions_of(node: &Node) -> Vec<Expression> {
    let Some(items) = node.as_seq() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let operator = match item.get("operator")?.as_str()? {
                "In" => Operator::In,
                "NotIn" => Operator::NotIn,
                "Exists" => Operator::Exists,
                "DoesNotExist" => Operator::DoesNotExist,
                _ => return None,
            };
            let values =
                item.get("values")
                    .and_then(Node::as_seq)
                    .map_or_else(Vec::new, |values| {
                        values
                            .iter()
                            .filter_map(|v| Some(v.as_str()?.to_string()))
                            .collect()
                    });
            Some(Expression {
                key: item.get("key")?.as_str()?.to_string(),
                operator,
                values,
            })
        })
        .collect()
}

/// The selectors an object states by `rows`. An empty selector selects
/// everything and says nothing: it is none.
pub fn selectors(found: &Found<'_>, enclosing: &Scope, rows: &[SelectorRow]) -> Vec<Selector> {
    let mut found_selectors = Vec::new();
    for row in rows {
        if !row.kinds.is_empty() && !row.kinds.contains(&found.object.id.kind.as_str()) {
            continue;
        }
        let path = FieldPath::parse(row.path).expect("a row's path is a field path");
        for hit in found.node.select(&path) {
            let labels = row.label_selector.then(|| hit.node.get("matchLabels"));
            let (pair_node, expressions) = match labels {
                Some(match_labels) => (
                    match_labels,
                    hit.node
                        .get("matchExpressions")
                        .map_or_else(Vec::new, expressions_of),
                ),
                None => (Some(hit.node), Vec::new()),
            };
            let pairs = pair_node.map_or_else(Vec::new, pairs_of);
            if pairs.is_empty() && expressions.is_empty() {
                continue;
            }
            found_selectors.push(Selector {
                site: pair_node.unwrap_or(hit.node).span.clone(),
                enclosing: enclosing.clone(),
                field: match (row.label_selector, pair_node) {
                    (false, _) => row.path.to_string(),
                    (true, Some(_)) => format!("{}.matchLabels", row.path),
                    (true, None) => format!("{}.matchExpressions", row.path),
                },
                matches: row.matches,
                target_kinds: row.target_kinds.iter().map(|k| (*k).to_string()).collect(),
                namespace: found.object.id.namespace.clone(),
                pairs,
                expressions,
            });
        }
    }
    found_selectors
}
