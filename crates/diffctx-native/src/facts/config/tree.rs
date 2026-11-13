//! The one shape every dialect is read into: a document is a [`Node`], a node is
//! null, a scalar, a sequence or a map, and each carries the span of the text it
//! was read from.

use serde::{Deserialize, Serialize};

use crate::facts::Span;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ScalarKind {
    Str,
    Int,
    Float,
    Bool,
    Datetime,
}

/// A scalar as the dialect decodes it: quotes, escapes and block scalars are
/// gone from `text`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Scalar {
    pub text: String,
    pub kind: ScalarKind,
    /// Written in quotes: `"80"` is a string whatever its text looks like.
    pub quoted: bool,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Value {
    Null,
    Scalar(Scalar),
    Seq(Vec<Node>),
    Map(Vec<Entry>),
}

/// A map key, kept in source order with its own span.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Key {
    pub text: String,
    pub span: Span,
    pub templated: bool,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub key: Key,
    pub value: Node,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Node {
    pub span: Span,
    pub value: Value,
    /// A scalar whose text a template wrote (see [`crate::facts::config::Templating`]):
    /// its `text` is a filler of the same length, never a literal. Always false
    /// for a sequence or a map.
    pub templated: bool,
}

impl Node {
    pub fn null(span: Span) -> Self {
        Self {
            span,
            value: Value::Null,
            templated: false,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self.value, Value::Null)
    }

    pub fn as_scalar(&self) -> Option<&Scalar> {
        match &self.value {
            Value::Scalar(scalar) => Some(scalar),
            _ => None,
        }
    }

    /// The text of a string scalar. A number or a boolean is not a string here,
    /// `name: 80` and `name: "80"` differ.
    pub fn as_str(&self) -> Option<&str> {
        self.as_scalar()
            .filter(|scalar| scalar.kind == ScalarKind::Str)
            .map(|scalar| scalar.text.as_str())
    }

    pub fn as_bool(&self) -> Option<bool> {
        let scalar = self.as_scalar().filter(|s| s.kind == ScalarKind::Bool)?;
        Some(matches!(scalar.text.as_str(), "true" | "True" | "TRUE"))
    }

    pub fn as_seq(&self) -> Option<&[Self]> {
        match &self.value {
            Value::Seq(items) => Some(items),
            _ => None,
        }
    }

    /// The entries of a map; none for any other node.
    pub fn entries(&self) -> &[Entry] {
        match &self.value {
            Value::Map(entries) => entries,
            _ => &[],
        }
    }

    /// The value of `key`: the last entry written with it (the reading of a
    /// non-strict JSON or YAML decoder), else the first map that a `<<` merge
    /// key brings in, an explicit key winning over a merged one and an earlier
    /// map of a merged sequence over a later one (the YAML merge key type).
    pub fn get(&self, key: &str) -> Option<&Self> {
        let entries = self.entries();
        entries
            .iter()
            .rev()
            .find(|entry| entry.key.text == key)
            .map(|entry| &entry.value)
            .or_else(|| {
                entries
                    .iter()
                    .filter(|entry| entry.key.text == MERGE_KEY)
                    .find_map(|entry| match &entry.value.value {
                        Value::Seq(maps) => maps.iter().find_map(|map| map.get(key)),
                        _ => entry.value.get(key),
                    })
            })
    }

    /// Every entry written with `key`, in source order, merges left out.
    pub fn get_all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a Self> + 'a {
        self.entries()
            .iter()
            .filter(move |entry| entry.key.text == key)
            .map(|entry| &entry.value)
    }

    /// `get` along a dotted path of keys: `metadata.name`.
    pub fn at(&self, path: &str) -> Option<&Self> {
        path.split('.').try_fold(self, |node, key| node.get(key))
    }

    /// The nodes a field path reaches, each with the node that holds it.
    pub fn select<'a>(&'a self, path: &FieldPath) -> Vec<Match<'a>> {
        let mut frontier: Vec<(&Self, &Self)> = vec![(self, self)];
        for segment in &path.0 {
            let mut next = Vec::new();
            for (_, node) in frontier {
                match segment {
                    Segment::Key(key) => {
                        if let Some(child) = node.get(key) {
                            next.push((node, child));
                        }
                    }
                    Segment::Each => {
                        if let Some(items) = node.as_seq() {
                            next.extend(items.iter().map(|item| (node, item)));
                        }
                    }
                }
            }
            frontier = next;
        }
        frontier
            .into_iter()
            .map(|(parent, node)| Match { parent, node })
            .collect()
    }

    /// The number of nodes in the tree, this one included.
    pub fn size(&self) -> usize {
        1 + match &self.value {
            Value::Seq(items) => items.iter().map(Self::size).sum(),
            Value::Map(entries) => entries.iter().map(|entry| entry.value.size()).sum(),
            Value::Null | Value::Scalar(_) => 0,
        }
    }
}

/// The key that merges the maps of its value into the map that holds it.
const MERGE_KEY: &str = "<<";

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Segment {
    Key(String),
    /// Every item of a sequence.
    Each,
}

/// Where a field sits under an object: `spec.template.spec.volumes[].secret.secretName`.
/// A segment is a key, `[]` after a key takes every item of the sequence it
/// holds. A key with a dot in it (an annotation) is not expressible.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FieldPath(Vec<Segment>);

impl FieldPath {
    /// `None` for an empty path or an empty segment.
    pub fn parse(path: &str) -> Option<Self> {
        let mut segments = Vec::new();
        for part in path.split('.') {
            let (key, each) = match part.strip_suffix("[]") {
                Some(key) => (key, true),
                None => (part, false),
            };
            if key.is_empty() {
                return None;
            }
            segments.push(Segment::Key(key.to_string()));
            if each {
                segments.push(Segment::Each);
            }
        }
        Some(Self(segments))
    }

    pub fn segments(&self) -> &[Segment] {
        &self.0
    }
}

/// A node a field path reached, and the map or sequence that holds it.
#[derive(Clone, Copy, Debug)]
pub struct Match<'a> {
    pub parent: &'a Node,
    pub node: &'a Node,
}
