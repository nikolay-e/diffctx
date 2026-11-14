//! The alias closure of a definition and the value positions of its occurrences.
//! A plain-name assignment is read from the spans around its target and value,
//! which the facts name; what no fact tells stays a candidate.

use std::ops::Range;

use super::{Alias, AliasBound, AliasScope, Basis, Outcome, Reason, ValuePlace};
use crate::facts::{DefKind, Fact, FileFacts, Language, LocalKey, Name, Scope, UseKind};
use crate::resolve::Relation;

/// Repeats `step` over each new scoped name until none appears: it accounts one
/// name (`None` for a reference name of the definition, else the alias's scope)
/// and returns the aliases it found. Their `hops` are set here. Stops at the
/// first alias past [`AliasBound::exceeded`] and returns that bound.
pub fn close_aliases<S>(names: &[Name], mut step: S) -> (Vec<Alias>, Option<AliasBound>)
where
    S: FnMut(&Name, Option<&AliasScope>) -> Vec<Alias>,
{
    let mut aliases: Vec<Alias> = Vec::new();
    let admit = |aliases: &mut Vec<Alias>, hops: usize, mut found: Vec<Alias>| {
        found.sort_by_cached_key(|alias| (alias.name.clone(), format!("{:?}", alias.scope)));
        for mut alias in found {
            if aliases
                .iter()
                .any(|known| known.name == alias.name && known.scope == alias.scope)
            {
                continue;
            }
            alias.hops = hops;
            aliases.push(alias);
            if let Some(bound) = AliasBound::exceeded(hops, aliases.len()) {
                return Some(bound);
            }
        }
        None
    };
    for name in names {
        if let Some(bound) = admit(&mut aliases, 1, step(name, None)) {
            return (aliases, Some(bound));
        }
    }
    let mut next = 0;
    while let Some(parent) = aliases.get(next).cloned() {
        let found = step(&parent.name, Some(&parent.scope));
        if let Some(bound) = admit(&mut aliases, parent.hops + 1, found) {
            return (aliases, Some(bound));
        }
        next += 1;
    }
    (aliases, None)
}

/// Families whose extractors read a name handed to a call as `PassAsValue`.
const ARGUMENT_FAMILIES: [Language; 7] = [
    Language::Python,
    Language::Rust,
    Language::Go,
    Language::JavaScript,
    Language::TypeScript,
    Language::Html,
    Language::Astro,
];

/// The scope a definition is declared in; a container no definition names reads
/// as the module (wider).
fn declared_in(file: &FileFacts, local: &LocalKey) -> Scope {
    let Some((owner, outer)) = local.container.split_last() else {
        return Scope::Module;
    };
    file.defs()
        .find(|def| def.local.name == *owner && def.local.container.as_slice() == outer)
        .map_or(Scope::Module, |def| Scope::Def(def.local.clone()))
}

fn byte_before(text: &[u8], start: u32) -> Option<u8> {
    text.get(..start as usize)?
        .iter()
        .rev()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
}

/// The name, its scope and whether it is a field or member store, of the target
/// of `target = value` where `value` is the occurrence alone: only the
/// assignment sign (and a `::` or `&`) lies between, the statement ends after
/// it, and no other assignment is chained to the target's left.
fn assigned_to(file: &FileFacts, text: &[u8], at: &Range<u32>) -> Option<(Name, Scope, bool)> {
    let rest = text.get(at.end as usize..)?;
    let end = rest
        .iter()
        .position(|byte| !matches!(byte, b' ' | b'\t' | b'\r'));
    if end.is_some_and(|end| !matches!(rest[end], b'\n' | b';')) {
        return None;
    }
    file.blob.facts.iter().find_map(|fact| {
        let (name, scope, bytes, store) = match fact {
            Fact::Definition(def)
                if matches!(
                    def.kind,
                    DefKind::Variable | DefKind::Constant | DefKind::Property | DefKind::Field
                ) =>
            {
                let scope = declared_in(file, &def.local);
                let field = matches!(def.kind, DefKind::Property | DefKind::Field);
                (&def.local.name, scope, &def.name_span.bytes, field)
            }
            Fact::LocalBinding(local) if local.site.bytes.len() == local.name.as_str().len() => (
                &local.name,
                local.enclosing.clone(),
                &local.site.bytes,
                false,
            ),
            Fact::Reference(write) if write.use_kind == UseKind::Write => (
                &write.name,
                write.enclosing.clone(),
                &write.site.bytes,
                !write.qualifier.is_empty(),
            ),
            _ => return None,
        };
        let gap: Vec<u8> = text
            .get(bytes.end as usize..at.start as usize)?
            .iter()
            .copied()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect();
        let before = byte_before(text, bytes.start);
        (matches!(gap.as_slice(), b"=" | b":=" | b"=::" | b"=&") && before != Some(b'='))
            .then(|| (name.clone(), scope, store || before == Some(b'.')))
    })
}

/// What the `ours` occurrence at `at` (a byte range of `text`) of a definition of
/// `kind` is where the definition is not called: an alias, a leak at a place the
/// facts tell, or, for a callable, a candidate; `None` leaves it `ours`. The
/// occurrence is matched to its reference by span equality. `relation` is the
/// binding's: an import, a re-export or an override is no value position.
pub fn value_use(
    file: &FileFacts,
    text: &[u8],
    at: &Range<u32>,
    kind: DefKind,
    relation: Option<Relation>,
) -> Option<(Outcome, Basis)> {
    if matches!(
        relation,
        Some(Relation::Imports | Relation::Reexports | Relation::Overrides)
    ) {
        return None;
    }
    let unplaced = || {
        matches!(
            kind,
            DefKind::Function | DefKind::Method | DefKind::Constructor
        )
        .then_some((Outcome::Candidate(Reason::Unexplained), Basis::Unproven))
    };
    let Some(use_kind) = file.blob.facts.iter().find_map(|fact| match fact {
        Fact::Reference(reference) if reference.site.bytes == *at => Some(reference.use_kind),
        _ => None,
    }) else {
        return unplaced();
    };
    if matches!(
        use_kind,
        UseKind::Call
            | UseKind::Instantiate
            | UseKind::Override
            | UseKind::Decorate
            | UseKind::Write
    ) {
        return None;
    }
    let alias = |name: &Name, scope: Scope, basis| {
        let scope = AliasScope::Declared(scope);
        let alias = Alias {
            name: name.clone(),
            scope,
            hops: 1,
        };
        Some((Outcome::Alias(alias), basis))
    };
    // Any mention on a type alias's right side counts: wider only adds names.
    let aliased = file
        .defs()
        .filter(|def| {
            let (span, name) = (&def.span.bytes, &def.name_span.bytes);
            def.kind == DefKind::TypeAlias
                && span.start <= at.start
                && at.end <= span.end
                && !(name.start <= at.start && at.end <= name.end)
        })
        .min_by_key(|def| def.span.bytes.len());
    if let Some(def) = aliased {
        return alias(
            &def.local.name,
            declared_in(file, &def.local),
            Basis::TypeAlias,
        );
    }
    if use_kind == UseKind::TypeUse {
        return None;
    }
    if let Some((name, scope, store)) = assigned_to(file, text, at) {
        return if store {
            Some((Outcome::Leak(ValuePlace::Store), Basis::ValuePosition))
        } else {
            alias(&name, scope, Basis::Assignment)
        };
    }
    if use_kind == UseKind::PassAsValue && ARGUMENT_FAMILIES.contains(&file.language()) {
        return Some((Outcome::Leak(ValuePlace::Argument), Basis::ValuePosition));
    }
    unplaced()
}
