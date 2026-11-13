//! The alias closure of a definition and the value positions of its occurrences.
//!
//! The closure is the loop of the contract: each scoped name the accounting
//! finds an alias in is accounted in turn, within the bound. [`value_use`] says,
//! from the facts of one file, what an `ours` occurrence that is not a call is:
//! a new name for the definition, a leak at a place the facts tell, or a value
//! use no fact places, which stays a candidate.
//!
//! What a family's facts cannot tell is never guessed: a plain-name assignment is
//! recognised by the spans around the target and the value (a fact names each),
//! not by a grammar. A shape the bytes do not prove stays unplaced.

use std::ops::Range;

use super::{Alias, AliasBound, AliasScope, Basis, Outcome, Reason, ValuePlace};
use crate::facts::{DefKind, Fact, FileFacts, Language, Lexical, LocalKey, Name, Scope, UseKind};
use crate::resolve::Relation;

/// The aliases a closure gathered, in the order it found them, and the bound it
/// went past, if it did: the pair is then partial, never complete.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct AliasClosure {
    pub aliases: Vec<Alias>,
    pub bound: Option<AliasBound>,
}

/// Repeats `step` over each new scoped name until none appears. `step` accounts
/// one name (`None` for a reference name of the definition, the alias's scope
/// for an alias) and returns the aliases it found; their `hops` are set here.
/// The closure stops at the first alias past [`AliasBound::exceeded`]. Aliases
/// of one step are taken in name order, so a run gives one answer.
pub fn close_aliases<S>(names: &[Name], mut step: S) -> AliasClosure
where
    S: FnMut(&Name, Option<&AliasScope>) -> Vec<Alias>,
{
    let mut closure = AliasClosure::default();
    for name in names {
        if !admit(&mut closure, 1, step(name, None)) {
            return closure;
        }
    }
    let mut next = 0;
    while let Some(parent) = closure.aliases.get(next).cloned() {
        let found = step(&parent.name, Some(&parent.scope));
        if !admit(&mut closure, parent.hops + 1, found) {
            break;
        }
        next += 1;
    }
    closure
}

/// Adds the aliases not seen before; `false` once the bound is passed. Hops only
/// grow along the queue, so the newest alias is the deepest.
fn admit(closure: &mut AliasClosure, hops: usize, mut found: Vec<Alias>) -> bool {
    found.sort_by_cached_key(order);
    for mut alias in found {
        let seen = closure
            .aliases
            .iter()
            .any(|known| known.name == alias.name && known.scope == alias.scope);
        if seen {
            continue;
        }
        alias.hops = hops;
        closure.aliases.push(alias);
        if let Some(bound) = AliasBound::exceeded(hops, closure.aliases.len()) {
            closure.bound = Some(bound);
            return false;
        }
    }
    true
}

fn order(alias: &Alias) -> (String, String) {
    let scope = match &alias.scope {
        AliasScope::File(path) => format!("0{}", path.as_str()),
        AliasScope::Importers(module) => format!("1{}", module.as_str()),
        AliasScope::Declared(scope) => format!("2{scope:?}"),
    };
    (alias.name.as_str().to_string(), scope)
}

/// What an `ours` occurrence is when the definition is not called there.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ValueUse {
    /// Not a value position: a call, a type, an import. It stays `ours`.
    Plain,
    /// The definition is given another name: accounted next, never a leak.
    Alias(Alias, Basis),
    /// Passed on as a value at a place the facts tell.
    Leak(ValuePlace),
    /// A value use of a callable at a place no fact tells. It must not stay
    /// `ours`: it blocks more, which is safe.
    Unplaced,
}

impl ValueUse {
    /// The outcome and basis that replace `ours`; `None` for [`ValueUse::Plain`].
    /// An unplaced use is a candidate, with the nearest reason there is until
    /// `Reason` names it.
    pub fn outcome(self) -> Option<(Outcome, Basis)> {
        match self {
            Self::Plain => None,
            Self::Alias(alias, basis) => Some((Outcome::Alias(alias), basis)),
            Self::Leak(place) => Some((Outcome::Leak(place), Basis::ValuePosition)),
            Self::Unplaced => Some((Outcome::Candidate(Reason::Unexplained), Basis::Unproven)),
        }
    }
}

/// How a family's extractor reads a name handed to a call.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reading {
    /// A script: `PassAsValue` is an argument (or a JSX attribute value) and an
    /// object pattern's `key: name` is read.
    Script,
    /// `PassAsValue` is an argument.
    Argument,
    /// `PassAsValue` is a reference form (`T::m`, `::f`, `f(...)`) at no place.
    Other,
}

const fn reading(language: Language) -> Reading {
    match language {
        Language::JavaScript | Language::TypeScript | Language::Html | Language::Astro => {
            Reading::Script
        }
        Language::Python | Language::Rust | Language::Go => Reading::Argument,
        Language::Kotlin
        | Language::Java
        | Language::Php
        | Language::C
        | Language::Cpp
        | Language::Config
        | Language::HelmTemplate
        | Language::HelmValues
        | Language::Terraform
        | Language::Dockerfile
        | Language::Shell
        | Language::ShellUnread
        | Language::Swift
        | Language::Sql => Reading::Other,
    }
}

/// The kinds `calls` applies to: a value use of one of them may be a call from
/// anywhere. For the others a plain read is no hedge.
const fn is_callable(kind: DefKind) -> bool {
    matches!(
        kind,
        DefKind::Function | DefKind::Method | DefKind::Constructor
    )
}

fn unplaced_for(kind: DefKind) -> ValueUse {
    if is_callable(kind) {
        ValueUse::Unplaced
    } else {
        ValueUse::Plain
    }
}

/// What the `ours` occurrence of a definition of `kind` at `at` (the byte range
/// of the name in `text`, the file's bytes) is, from the facts of `file`.
/// `relation` is the binding's, when it has one: an import or a re-export is no
/// value position. The occurrence is matched to its reference by span equality,
/// never by containment; with none, a callable's use is unplaced.
pub fn value_use(
    file: &FileFacts,
    text: &[u8],
    at: &Range<u32>,
    kind: DefKind,
    relation: Option<Relation>,
) -> ValueUse {
    if matches!(relation, Some(Relation::Imports | Relation::Reexports)) {
        return ValueUse::Plain;
    }
    let Some(use_kind) = use_kind_at(file, at) else {
        return unplaced_for(kind);
    };
    match use_kind {
        UseKind::Call
        | UseKind::Instantiate
        | UseKind::Override
        | UseKind::Decorate
        | UseKind::Write => ValueUse::Plain,
        UseKind::TypeUse => type_alias(file, at).map_or(ValueUse::Plain, |alias| {
            ValueUse::Alias(alias, Basis::TypeAlias)
        }),
        UseKind::Read | UseKind::PassAsValue => {
            value_position(file, text, at, use_kind == UseKind::PassAsValue, kind)
        }
    }
}

fn use_kind_at(file: &FileFacts, at: &Range<u32>) -> Option<UseKind> {
    file.blob.facts.iter().find_map(|fact| match fact {
        Fact::Reference(reference) if reference.site.bytes == *at => Some(reference.use_kind),
        _ => None,
    })
}

fn value_position(
    file: &FileFacts,
    text: &[u8],
    at: &Range<u32>,
    passed: bool,
    kind: DefKind,
) -> ValueUse {
    if let Some(alias) = type_alias(file, at) {
        return ValueUse::Alias(alias, Basis::TypeAlias);
    }
    if let Some(target) = assigned_to(file, text, at) {
        return if target.store {
            ValueUse::Leak(ValuePlace::Store)
        } else {
            ValueUse::Alias(target.alias(), Basis::Assignment)
        };
    }
    let how = reading(file.language());
    if how == Reading::Script {
        if let Some(alias) = destructured_into(file, text, at) {
            return ValueUse::Alias(alias, Basis::Destructuring);
        }
    }
    if passed && how != Reading::Other {
        return ValueUse::Leak(ValuePlace::Argument);
    }
    unplaced_for(kind)
}

/// The name a plain assignment gives, or the field or the member it stores to.
struct Target {
    name: Name,
    scope: Scope,
    bytes: Range<u32>,
    /// A field or a member (`a.b = f`), not a plain name.
    store: bool,
}

impl Target {
    fn alias(self) -> Alias {
        Alias {
            name: self.name,
            scope: AliasScope::Declared(self.scope),
            hops: 1,
        }
    }
}

/// Every name the facts say is bound or written: a variable, constant, property
/// or field definition, a local whose site is its name, a write.
fn targets(file: &FileFacts, text: &[u8]) -> Vec<Target> {
    let mut found = Vec::new();
    for fact in &file.blob.facts {
        match fact {
            Fact::Definition(def)
                if matches!(
                    def.kind,
                    DefKind::Variable | DefKind::Constant | DefKind::Property | DefKind::Field
                ) =>
            {
                found.push(Target {
                    name: def.local.name.clone(),
                    scope: declared_in(file, &def.local),
                    bytes: def.name_span.bytes.clone(),
                    store: byte_before(text, def.name_span.bytes.start) == Some(b'.'),
                });
            }
            Fact::LocalBinding(local) if local.site.bytes.len() == local.name.as_str().len() => {
                found.push(Target {
                    name: local.name.clone(),
                    scope: local.enclosing.clone(),
                    bytes: local.site.bytes.clone(),
                    store: false,
                });
            }
            Fact::Reference(write) if write.use_kind == UseKind::Write => {
                found.push(Target {
                    name: write.name.clone(),
                    scope: write.enclosing.clone(),
                    bytes: write.site.bytes.clone(),
                    store: !write.qualifier.is_empty()
                        || byte_before(text, write.site.bytes.start) == Some(b'.'),
                });
            }
            _ => {}
        }
    }
    found
}

/// The scope a definition is declared in: its container's definition, or the
/// module's own. A container no definition names is read as the module (wider).
fn declared_in(file: &FileFacts, local: &LocalKey) -> Scope {
    let Some((owner, outer)) = local.container.split_last() else {
        return Scope::Module;
    };
    file.defs()
        .find(|def| def.local.name == *owner && def.local.container.as_slice() == outer)
        .map_or(Scope::Module, |def| Scope::Def(def.local.clone()))
}

fn covers(outer: &Range<u32>, inner: &Range<u32>) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

/// The last byte before `start` that is not whitespace.
fn byte_before(text: &[u8], start: u32) -> Option<u8> {
    text.get(..start as usize)?
        .iter()
        .rev()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
}

/// The bytes of `text` from `from` to `to`, with no whitespace.
fn squeezed(text: &[u8], from: u32, to: u32) -> Option<Vec<u8>> {
    let gap = text.get(from as usize..to as usize)?;
    Some(
        gap.iter()
            .copied()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect(),
    )
}

/// Whether the value ends its statement: a newline, a `;`, the end of the file
/// or a comment follows it.
fn ends_statement(file: &FileFacts, text: &[u8], from: u32) -> bool {
    let mut at = from as usize;
    while let Some(byte) = text.get(at) {
        match byte {
            b' ' | b'\t' | b'\r' => at += 1,
            b'\n' | b';' => return true,
            _ => return file.blob.lexical_at(at as u32) == Some(Lexical::Comment),
        }
    }
    true
}

/// The target of `target = value` when `value` is the occurrence alone: nothing
/// but the assignment sign (and a `::` or `&` before the value) lies between,
/// and no other assignment is chained to the target's left.
fn assigned_to(file: &FileFacts, text: &[u8], at: &Range<u32>) -> Option<Target> {
    if !ends_statement(file, text, at.end) {
        return None;
    }
    targets(file, text).into_iter().find(|target| {
        target.bytes.end <= at.start
            && squeezed(text, target.bytes.end, at.start)
                .is_some_and(|gap| matches!(gap.as_slice(), b"=" | b":=" | b"=::" | b"=&"))
            && byte_before(text, target.bytes.start) != Some(b'=')
    })
}

/// The name an object pattern gives the member read at `at`: `{ f: g }`, the
/// target on the same line after a lone colon.
fn destructured_into(file: &FileFacts, text: &[u8], at: &Range<u32>) -> Option<Alias> {
    targets(file, text)
        .into_iter()
        .find(|target| {
            let between = text.get(at.end as usize..target.bytes.start as usize);
            !target.store
                && target.bytes.start >= at.end
                && between.is_some_and(|gap| !gap.contains(&b'\n'))
                && squeezed(text, at.end, target.bytes.start).is_some_and(|gap| gap == b":")
        })
        .map(Target::alias)
}

/// The type alias whose right side holds the occurrence: any mention there is
/// taken as the aliased type, which only adds names (`type G = Vec<F>` too).
fn type_alias(file: &FileFacts, at: &Range<u32>) -> Option<Alias> {
    file.defs()
        .filter(|def| {
            def.kind == DefKind::TypeAlias
                && covers(&def.span.bytes, at)
                && !covers(&def.name_span.bytes, at)
        })
        .min_by_key(|def| def.span.bytes.len())
        .map(|def| Alias {
            name: def.local.name.clone(),
            scope: AliasScope::Declared(declared_in(file, &def.local)),
            hops: 1,
        })
}
