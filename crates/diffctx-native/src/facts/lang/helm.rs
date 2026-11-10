//! What a Helm chart's files state.
//!
//! - A template (`templates/**.yaml`, `*.tpl`) is read around its Go template
//!   actions. The named templates it defines (`define "name"`) are definitions of
//!   kind `Function`, from the action to its `end`; the named templates it
//!   includes and the values it reads are `ConfigUse`s, owned by the named
//!   template they sit in, else by the module. Its objects are the config
//!   cell's own objects where their identity is written, and a template writes it
//!   in most: those references are the module's.
//! - A values file is a tree of keys: each key, at any depth, is a definition of
//!   kind `Field` whose container is the keys above it. A key's hash covers
//!   everything under it, so an edit of a leaf is an edit of the key and of every
//!   key above it, and a reader of `image` and a reader of `image.tag` are each
//!   reached by the edits that concern them.

use std::cell::Cell;
use std::collections::HashMap;

use crate::facts::config::{self, ConfigUse, Node, Options, Templating, Value};
use crate::facts::{
    BlobFacts, Def, DefKind, Disambig, Extractor, Fact, Language, LocalKey, Name, Namespace, Scope,
    Span, Visibility, hash_text, hash_tokens,
};
use crate::snapshot::RunBudget;

use super::config::{blob_of, facts_of_reading, slice, tree_hash};

pub struct HelmTemplateExtractor;

impl Extractor for HelmTemplateExtractor {
    fn language(&self) -> Language {
        Language::HelmTemplate
    }

    fn extract(&self, text: &[u8], budget: &RunBudget) -> BlobFacts {
        blob_of(
            Language::HelmTemplate,
            text,
            budget,
            Options {
                templating: Templating::Go,
            },
            |reading, source, cut| {
                let mut facts = facts_of_reading(reading, source, true, cut);
                facts.extend(template_facts(reading, source));
                facts
            },
        )
    }
}

fn template_facts(reading: &config::Reading, source: &str) -> Vec<Fact> {
    let analysis = config::analyse(&reading.templates);
    let mut facts = Vec::new();
    let mut seen: HashMap<&str, u16> = HashMap::new();
    let mut locals: Vec<LocalKey> = Vec::new();
    for define in &analysis.defines {
        let nth = seen.entry(define.name.as_str()).or_insert(0);
        let mut local = LocalKey::new(Vec::new(), define.name.as_str(), Namespace::Value);
        if *nth > 0 {
            local = local.with_disambig(Disambig::Overload(*nth));
        }
        *nth = nth.saturating_add(1);
        let text = slice(source, &define.span);
        facts.push(Fact::Definition(Def {
            local: local.clone(),
            kind: DefKind::Function,
            span: define.span.clone(),
            name_span: define.name_span.clone(),
            signature: None,
            decorators: Vec::new(),
            visibility: Visibility::Public,
            body_hash: hash_tokens(text.split_whitespace().map(str::as_bytes)).unwrap_or(0),
            text_hash: hash_text(text.as_bytes()),
            doc_hash: None,
        }));
        locals.push(local);
    }
    for found in analysis.uses {
        let enclosing = match found.inside {
            Some(index) => Scope::Def(locals[index].clone()),
            None => Scope::Module,
        };
        facts.push(Fact::ConfigUse(ConfigUse {
            site: found.site,
            enclosing,
            of: found.of,
            name: found.name,
        }));
    }
    facts
}

pub struct HelmValuesExtractor;

impl Extractor for HelmValuesExtractor {
    fn language(&self) -> Language {
        Language::HelmValues
    }

    fn extract(&self, text: &[u8], budget: &RunBudget) -> BlobFacts {
        blob_of(
            Language::HelmValues,
            text,
            budget,
            Options::default(),
            |reading, source, cut| {
                let mut facts = Vec::new();
                if let Some(root) = reading.documents.first() {
                    walk(root, &mut Vec::new(), source, &mut facts, cut);
                }
                facts
            },
        )
    }
}

/// Keys of a values file are followed this deep.
const MAX_KEY_DEPTH: usize = 12;

fn walk(node: &Node, path: &mut Vec<Name>, source: &str, facts: &mut Vec<Fact>, cut: &Cell<bool>) {
    for entry in node.entries() {
        if entry.key.text == "<<" {
            continue;
        }
        let local = LocalKey::new(path.clone(), entry.key.text.as_str(), Namespace::Value);
        let span = Span::new(
            entry.key.span.bytes.start..entry.value.span.bytes.end.max(entry.key.span.bytes.end),
            (
                entry.key.span.lines.0,
                entry.value.span.lines.1.max(entry.key.span.lines.0),
            ),
        );
        facts.push(Fact::Definition(Def {
            local,
            kind: DefKind::Field,
            name_span: entry.key.span.clone(),
            signature: None,
            decorators: Vec::new(),
            visibility: Visibility::Public,
            body_hash: tree_hash(&entry.value),
            text_hash: hash_text(slice(source, &span).as_bytes()),
            doc_hash: None,
            span,
        }));
        if matches!(entry.value.value, Value::Map(_)) {
            if path.len() < MAX_KEY_DEPTH {
                path.push(Name::new(entry.key.text.clone()));
                walk(&entry.value, path, source, facts, cut);
                path.pop();
            } else {
                cut.set(true);
            }
        }
    }
}
