//! What a YAML file states as configuration: the objects it holds, the objects
//! its fields name, the places it points at.
//!
//! - An object (`apiVersion`, `kind`, `metadata.name`) is a definition of kind
//!   `Object`, keyed `Kind/name` (`Kind.group/name` outside the core group) with
//!   its namespace in the disambiguator (`config::local_key`). Its body hash is
//!   over the whole object, so any change to its content is a change to it and a
//!   re-indent or a comment is layout.
//! - A field a row of the Kubernetes table names, and a `*Name` or `*Ref` field no
//!   row names, is a `ConfigRef` owned by the object that writes it; the second is
//!   a candidate by its strength.
//! - A label selector (a Service's `selector`, a `matchLabels`) is a
//!   `ConfigSelector`, and the labels of an object are definitions of their own,
//!   `labels` and for a workload `pod-labels`, each with its pairs in a
//!   `ConfigLabels`: an edit of the labels is an edit of that definition, and any
//!   other edit of the object leaves the selectors alone.
//! - A place a Kustomize, Argo CD, Flux, Helmfile or chart file names is a
//!   `ConfigPointer`, a path of the repository or a source outside it alike.
//!
//! The file is read as plain YAML. A Helm template is not: its reading has parse
//! errors, and the facts around them are still given.

use std::cell::Cell;
use std::collections::HashMap;
use std::ops::Range;

use crate::facts::config::{
    self, ConfigRef, Dialect, Flaw, Labels, Node, ObjectId, Options, ScalarKind, Strength, Value,
    candidates, k8s, local_key, objects, pairs_of, pointers_of, releases_of, selectors,
};
use crate::facts::{
    BlobFacts, Def, DefKind, Extractor, Fact, Hasher, Language, LocalKey, Namespace, Scope, Span,
    Visibility, hash_text,
};
use crate::snapshot::{Loss, RepoPath, RunBudget};

pub struct ConfigExtractor;

fn unbound() -> RepoPath {
    RepoPath::new("<blob>").expect("a valid path")
}

impl Extractor for ConfigExtractor {
    fn language(&self) -> Language {
        Language::Config
    }

    fn extract(&self, text: &[u8], budget: &RunBudget) -> BlobFacts {
        blob_of(
            Language::Config,
            text,
            budget,
            Options::default(),
            |reading, source, cut| facts_of_reading(reading, source, false, cut),
        )
    }
}

/// The blob of a YAML file read with `options`: its facts from `facts`, and
/// every way the read stopped short as a loss, those `facts` sets its cut
/// flag for included.
pub(super) fn blob_of(
    language: Language,
    text: &[u8],
    budget: &RunBudget,
    options: Options,
    facts: impl FnOnce(&config::Reading, &str, &Cell<bool>) -> Vec<Fact>,
) -> BlobFacts {
    let reading = config::read(Dialect::Yaml, text, options, budget);
    let mut blob = BlobFacts::new(language);
    if !reading.errors.is_empty() {
        blob.parse_error(reading.errors.clone());
    }
    for flaw in &reading.flaws {
        match flaw {
            Flaw::NotUtf8 | Flaw::Unsupported => {
                blob.losses.push(Loss::Unsupported { path: unbound() });
            }
            Flaw::Deadline => blob.deadline(),
            Flaw::TooDeep | Flaw::TooLarge => {
                blob.losses.push(Loss::ExtractorFailed { path: unbound() });
            }
            // Its line is among the reading's errors, so it is a parse error
            // above, named and placed.
            Flaw::UnresolvedAlias { .. } => {}
        }
    }
    let source = std::str::from_utf8(text).unwrap_or("");
    let cut = Cell::new(false);
    blob.facts = facts(&reading, source, &cut);
    let failed = |l: &Loss| matches!(l, Loss::ExtractorFailed { .. });
    if cut.get() && !blob.losses.iter().any(failed) {
        blob.losses.push(Loss::ExtractorFailed { path: unbound() });
    }
    blob.in_source_order();
    blob
}

/// The facts of the objects, pointers and releases of a reading. `rendered`: the
/// file is a template, whose objects are written by a chart in many releases: an
/// object whose identity a template writes is no definition, and the references
/// it states are the module's.
pub(super) fn facts_of_reading(
    reading: &config::Reading,
    source: &str,
    rendered: bool,
    cut: &Cell<bool>,
) -> Vec<Fact> {
    let mut facts = Vec::new();
    let mut earlier: HashMap<ObjectId, u16> = HashMap::new();
    for found in objects(reading) {
        let id = found.object.id.clone();
        if found.object.templated {
            if rendered {
                let typed = config::extract(&found, &Scope::Module, k8s::references());
                facts.extend(typed.into_iter().map(Fact::ConfigRef));
            }
            continue;
        }
        let count = earlier.entry(id.clone()).or_insert(0);
        let nth = *count;
        *count = count.saturating_add(1);
        let local = local_key(&id, nth);
        let enclosing = Scope::Def(local.clone());
        let (parent_name, parent_disambig) = (local.name.clone(), local.disambig.clone());
        facts.push(Fact::Definition(Def {
            local,
            kind: DefKind::Object,
            span: found.object.site.clone(),
            name_span: found.object.name_site.clone(),
            signature: Some(format!("{}/{} {}", id.group, found.object.version, id.kind)),
            decorators: Vec::new(),
            visibility: Visibility::Public,
            body_hash: tree_hash(found.node),
            text_hash: hash_text(slice(source, &found.object.site).as_bytes()),
            doc_hash: None,
        }));
        let typed = config::extract(&found, &enclosing, k8s::references());
        let sites: Vec<Range<u32>> = typed.iter().map(|r| r.site.bytes.clone()).collect();
        facts.extend(typed.into_iter().map(Fact::ConfigRef));
        let (named, stopped) = candidates(found.node, &sites);
        if stopped {
            cut.set(true);
        }
        for candidate in named {
            let Some(name) = candidate.name.as_str() else {
                continue;
            };
            facts.push(Fact::ConfigRef(ConfigRef {
                site: candidate.name.span.clone(),
                enclosing: enclosing.clone(),
                from: id.clone(),
                field: candidate.field.to_string(),
                target_kind: String::new(),
                target_name: name.to_string(),
                target_namespace: None,
                key: None,
                strength: Strength::Candidate,
                templated: false,
            }));
        }
        facts.extend(
            selectors(&found, &enclosing, k8s::selector_rows())
                .into_iter()
                .map(Fact::ConfigSelector),
        );
        let own = found.node.at("metadata.labels");
        let pod = k8s::pod_labels_path(&id.kind).and_then(|path| found.node.at(path));
        for (name, labels) in [("labels", own), ("pod-labels", pod)] {
            let Some(labels) = labels else {
                continue;
            };
            let pairs = pairs_of(labels);
            if pairs.is_empty() {
                continue;
            }
            let mut key = LocalKey::new(vec![parent_name.clone()], name, Namespace::Value);
            key.disambig.clone_from(&parent_disambig);
            facts.push(Fact::Definition(Def {
                local: key.clone(),
                kind: DefKind::Field,
                span: labels.span.clone(),
                name_span: labels.span.clone(),
                signature: None,
                decorators: Vec::new(),
                visibility: Visibility::Public,
                body_hash: tree_hash(labels),
                text_hash: hash_text(slice(source, &labels.span).as_bytes()),
                doc_hash: None,
            }));
            facts.push(Fact::ConfigLabels(Labels {
                site: labels.span.clone(),
                enclosing: Scope::Def(key),
                pairs,
            }));
        }
    }
    for document in &reading.documents {
        facts.extend(pointers_of(document).into_iter().map(Fact::ConfigPointer));
        for release in releases_of(document) {
            // Argo CD's `path` is read from the release, not from the pointer
            // rows: it is relative to the source repository's root.
            if release.values_from_chart {
                facts.extend(release.chart.clone().map(Fact::ConfigPointer));
            }
            facts.push(Fact::ConfigRelease(release));
        }
    }
    facts
}

pub(super) fn slice<'a>(source: &'a str, span: &Span) -> &'a str {
    source
        .get(span.bytes.start as usize..span.bytes.end as usize)
        .unwrap_or("")
}

/// The object's tree, without layout: kinds, nesting, keys and leaf text.
pub(super) fn tree_hash(node: &Node) -> u64 {
    let mut hasher = Hasher::new();
    mix(node, &mut hasher);
    hasher.finish().unwrap_or(1)
}

fn mix(node: &Node, hasher: &mut Hasher) {
    match &node.value {
        Value::Null => {
            hasher.token(b"~");
        }
        Value::Scalar(scalar) => {
            let tag: &[u8] = match scalar.kind {
                ScalarKind::Str => b"s",
                ScalarKind::Int => b"i",
                ScalarKind::Float => b"f",
                ScalarKind::Bool => b"b",
                ScalarKind::Datetime => b"d",
            };
            hasher.token(tag).token(scalar.text.as_bytes());
        }
        Value::Seq(items) => {
            hasher.token(b"[");
            for item in items {
                mix(item, hasher);
            }
            hasher.token(b"]");
        }
        Value::Map(entries) => {
            hasher.token(b"{");
            for entry in entries {
                hasher.token(entry.key.text.as_bytes());
                mix(&entry.value, hasher);
            }
            hasher.token(b"}");
        }
    }
}
