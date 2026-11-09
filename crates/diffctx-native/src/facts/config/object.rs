//! The objects a document states: a Kubernetes-style `apiVersion`, `kind` and
//! `metadata.name`, which also name a Helm chart's rendered files, Kustomize,
//! Argo CD and Flux resources.

use serde::{Deserialize, Serialize};

use crate::facts::{Disambig, LocalKey, Name, Namespace, Span};

use super::Reading;
use super::tree::Node;

/// What a Kubernetes object is: its API group, its kind, its namespace if it
/// stands in one, and its name. Names are unique per (group, kind, namespace)
/// whatever the API version; the version is not part of the identity.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct ObjectId {
    /// `apps` for `apps/v1`; empty for the core group (`v1`).
    pub group: String,
    pub kind: String,
    /// The namespace the file writes. `None` is a file that says none, which a
    /// cluster-scoped kind and a namespaced one given its namespace at deploy
    /// time have alike: the resolver of a family tells them apart.
    pub namespace: Option<String>,
    pub name: String,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Object {
    pub id: ObjectId,
    /// `v1` of `apps/v1`.
    pub version: String,
    /// The mapping that states it.
    pub site: Span,
    /// The name as written.
    pub name_site: Span,
    /// Some part of the identity is written by a template: `id` holds filler
    /// and names no object.
    pub templated: bool,
}

/// An object and the node that states it.
#[derive(Clone, Debug)]
pub struct Found<'a> {
    pub object: Object,
    pub node: &'a Node,
}

/// The objects of a read, in source order, with the nodes that state them.
/// A `List` (or any kind that ends in it) states the objects of its `items`
/// and none of its own; a document without an `apiVersion`, a `kind` and a
/// string `metadata.name` states none.
pub fn objects(reading: &Reading) -> Vec<Found<'_>> {
    let mut found = Vec::new();
    for document in &reading.documents {
        collect(document, &mut found);
    }
    found
}

fn collect<'a>(node: &'a Node, found: &mut Vec<Found<'a>>) {
    let kind = node.get("kind").and_then(Node::as_str);
    let items = node.get("items").and_then(Node::as_seq);
    if let (Some(kind), Some(items)) = (kind, items) {
        if kind.ends_with("List") {
            for item in items {
                collect(item, found);
            }
            return;
        }
    }
    if let Some(object) = object_of(node) {
        found.push(Found { object, node });
    }
}

fn object_of(node: &Node) -> Option<Object> {
    let api_version = node.get("apiVersion")?;
    let kind = node.get("kind")?;
    let metadata = node.get("metadata")?;
    let name = metadata.get("name")?;
    let namespace = metadata.get("namespace");
    let api = api_version.as_str()?;
    let (group, version) = api.split_once('/').unwrap_or(("", api));
    let namespace_text = match namespace {
        Some(namespace) if !namespace.is_null() => Some(namespace.as_str()?.to_string()),
        _ => None,
    };
    Some(Object {
        id: ObjectId {
            group: group.to_string(),
            kind: kind.as_str()?.to_string(),
            namespace: namespace_text,
            name: name.as_str()?.to_string(),
        },
        version: version.to_string(),
        site: node.span.clone(),
        name_site: name.span.clone(),
        templated: [Some(api_version), Some(kind), Some(name), namespace]
            .into_iter()
            .flatten()
            .any(|part| part.templated),
    })
}

/// The key of the definition an object is in its file: `Kind/name` for the core
/// group, `Kind.group/name` for any other (as `kubectl` spells a resource), and
/// its namespace and the number of earlier objects of the same identity in the
/// file in the disambiguator. A namespace is part of the key because
/// `Service/api` of two namespaces are two objects.
pub fn local_key(id: &ObjectId, earlier: u16) -> LocalKey {
    let name = if id.group.is_empty() {
        format!("{}/{}", id.kind, id.name)
    } else {
        format!("{}.{}/{}", id.kind, id.group, id.name)
    };
    let key = LocalKey::new(Vec::new(), Name::new(name), Namespace::Value);
    match (&id.namespace, earlier) {
        (None, 0) => key,
        (namespace, n) => {
            let namespace = namespace.as_deref().unwrap_or("");
            let tag = if n > 0 {
                format!("ns:{namespace}#{n}")
            } else {
                format!("ns:{namespace}")
            };
            key.with_disambig(Disambig::Cfg(tag))
        }
    }
}

/// The identity a key written by [`local_key`] stands for; `None` for any other.
pub fn identity_of(key: &LocalKey) -> Option<ObjectId> {
    let (head, name) = key.name.as_str().split_once('/')?;
    let (kind, group) = head.split_once('.').unwrap_or((head, ""));
    let namespace = match &key.disambig {
        None => None,
        Some(Disambig::Cfg(tag)) => {
            let namespace = tag.strip_prefix("ns:")?;
            let namespace = namespace.split_once('#').map_or(namespace, |(ns, _)| ns);
            (!namespace.is_empty()).then(|| namespace.to_string())
        }
        Some(_) => return None,
    };
    Some(ObjectId {
        group: group.to_string(),
        kind: kind.to_string(),
        namespace,
        name: name.to_string(),
    })
}
