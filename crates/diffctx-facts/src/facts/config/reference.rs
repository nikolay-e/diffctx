//! References from one object to another, read through a table: a row says that
//! a field at a path, in objects of some kinds, names an object of a kind. The
//! rows are data (`super::k8s` holds the Kubernetes table); this is the
//! machinery that reads them.
//!
//! A field is a reference because a row names it. A key that merely ends in
//! `Ref` or `Name` is at most a [`Strength::Candidate`], which `super::candidates`
//! finds and a row of that strength would say explicitly.

use serde::{Deserialize, Serialize};

use crate::facts::{Scope, Span};

use super::object::{Found, ObjectId};
use super::tree::FieldPath;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Strength {
    /// A row of the table names the field.
    Typed,
    /// The field looks like a reference and no row says it is one.
    Candidate,
}

/// One row: in an object of one of `kinds` (any kind when empty), the string at
/// `path` is the name of an object of kind `target`.
#[derive(Clone, Copy, Debug)]
pub struct Row {
    pub kinds: &'static [&'static str],
    /// `spec.volumes[].configMap.name`: see [`FieldPath`].
    pub path: &'static str,
    pub target: &'static str,
    /// A field beside the one at `path`, in the map that holds it, that names a
    /// key inside the target (`configMapKeyRef`'s `key`).
    pub key: Option<&'static str>,
    /// A field beside the one at `path` that names the target's namespace.
    pub namespace: Option<&'static str>,
    /// A field beside the one at `path` that names the target's kind
    /// (`roleRef.kind`, `scaleTargetRef.kind`), in place of `target`.
    pub kind_field: Option<&'static str>,
    /// The row applies only where a field beside the one at `path` has a value
    /// (`subjects[].kind` is `ServiceAccount`).
    pub only_when: Option<(&'static str, &'static str)>,
    pub strength: Strength,
}

impl Row {
    /// A typed row with nothing beside the field.
    pub const fn new(
        kinds: &'static [&'static str],
        path: &'static str,
        target: &'static str,
    ) -> Self {
        Self {
            kinds,
            path,
            target,
            key: None,
            namespace: None,
            kind_field: None,
            only_when: None,
            strength: Strength::Typed,
        }
    }

    #[must_use]
    pub const fn key(mut self, field: &'static str) -> Self {
        self.key = Some(field);
        self
    }

    #[must_use]
    pub const fn namespace(mut self, field: &'static str) -> Self {
        self.namespace = Some(field);
        self
    }

    #[must_use]
    pub const fn kind_from(mut self, field: &'static str) -> Self {
        self.kind_field = Some(field);
        self
    }

    #[must_use]
    pub const fn when(mut self, field: &'static str, value: &'static str) -> Self {
        self.only_when = Some((field, value));
        self
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ConfigRef {
    /// The name's own text in the file.
    pub site: Span,
    /// The definition of the object that writes the reference.
    pub enclosing: Scope,
    pub from: ObjectId,
    /// The row's path, as the table writes it; the key's name for a candidate.
    pub field: String,
    /// The kind the field names; empty for a candidate, which may be any.
    pub target_kind: String,
    pub target_name: String,
    /// The namespace the field writes, not the one it defaults to: the object's
    /// own namespace is `from.namespace`.
    pub target_namespace: Option<String>,
    pub key: Option<String>,
    pub strength: Strength,
    /// A template wrote the name or the namespace: they hold filler and name
    /// nothing.
    pub templated: bool,
}

/// The references an object states by `rows`. A field that is not a string
/// (null, a number, a map) is no reference.
pub fn extract(found: &Found<'_>, enclosing: &Scope, rows: &[Row]) -> Vec<ConfigRef> {
    let mut refs = Vec::new();
    for row in rows {
        if !row.kinds.is_empty() && !row.kinds.contains(&found.object.id.kind.as_str()) {
            continue;
        }
        let path = FieldPath::parse(row.path).expect("a row's path is a field path");
        for hit in found.node.select(&path) {
            let Some(name) = hit.node.as_str() else {
                continue;
            };
            let beside = |field: Option<&str>| {
                field
                    .and_then(|field| hit.parent.get(field))
                    .filter(|node| node.as_str().is_some())
            };
            if let Some((field, value)) = row.only_when {
                if beside(Some(field)).and_then(|node| node.as_str()) != Some(value) {
                    continue;
                }
            }
            let target = match row.kind_field {
                Some(field) => match beside(Some(field)).and_then(|node| node.as_str()) {
                    Some(kind) => kind,
                    None => continue,
                },
                None => row.target,
            };
            let namespace = beside(row.namespace);
            let key = beside(row.key);
            refs.push(ConfigRef {
                site: hit.node.span.clone(),
                enclosing: enclosing.clone(),
                from: found.object.id.clone(),
                field: row.path.to_string(),
                target_kind: target.to_string(),
                target_name: name.to_string(),
                target_namespace: namespace.and_then(|n| n.as_str()).map(str::to_string),
                key: key.and_then(|n| n.as_str()).map(str::to_string),
                strength: row.strength,
                templated: hit.node.templated || namespace.is_some_and(|n| n.templated),
            });
        }
    }
    refs
}
