//! The alias closure of a definition and the value positions of its occurrences.
//! A plain-name assignment is read from the spans around its target and value,
//! which the facts name; what no fact tells stays a candidate.

use std::ops::Range;

use super::rows::STATEMENT_KEYWORDS;
use super::{Alias, AliasBound, AliasScope, Basis, Outcome, Reason, ValuePlace};
use crate::facts::config::UseOf;
use crate::facts::{DefKind, Fact, FileFacts, Language, LocalKey, LocalKind, Name, Scope, UseKind};
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
const ARGUMENT_FAMILIES: [Language; 8] = [
    Language::Python,
    Language::Rust,
    Language::Go,
    Language::Kotlin,
    Language::JavaScript,
    Language::TypeScript,
    Language::Html,
    Language::Astro,
];

/// Families whose extractors read the shorthand of an object pattern at the span of the local it binds.
const PATTERN_FAMILIES: [Language; 2] = [Language::JavaScript, Language::TypeScript];

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
        (assignment(&gap) && before != Some(b'='))
            .then(|| (name.clone(), scope, store || before == Some(b'.')))
    })
}

/// Whether the bytes between an assignment's target and an occurrence, whitespace
/// dropped, are its operator: alone, or before the written qualifier the occurrence
/// is the last name of (`fetch = Svc.fetch`).
fn assignment(gap: &[u8]) -> bool {
    if matches!(gap, b"=" | b":=" | b"=::" | b"=&") {
        return true;
    }
    let qualifier = gap.strip_prefix(b":=").or_else(|| gap.strip_prefix(b"="));
    qualifier.is_some_and(|chain| {
        (chain.ends_with(b".") || chain.ends_with(b"::"))
            && chain.iter().any(u8::is_ascii_alphanumeric)
            && chain
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_$.:".contains(byte))
    })
}

/// The callee whose argument list holds the occurrence at `at`: the dotted path before
/// the nearest unclosed `(`, when the text names one. A statement's own group (`return (…)`,
/// `if (…)`) is no call: its operands are no argument of any callee.
pub fn callee_of(text: &[u8], at: &Range<u32>) -> Option<Name> {
    const REACH: usize = 2000;
    let start = (at.start as usize).min(text.len());
    let mut depth = 0usize;
    let open = (text[start.saturating_sub(REACH)..start])
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, byte)| match byte {
            b')' | b']' | b'}' => {
                depth += 1;
                None
            }
            b'(' | b'[' | b'{' if depth > 0 => {
                depth -= 1;
                None
            }
            b'(' => Some(index),
            _ => None,
        })?
        + start.saturating_sub(REACH);
    let before = &text[..open];
    let head = before.len() - before.trim_ascii_end().len();
    let word = &before[..before.len() - head];
    let begin = word
        .iter()
        .rposition(|byte| !(byte.is_ascii_alphanumeric() || b"_$.:".contains(byte)))
        .map_or(0, |at| at + 1);
    let path = std::str::from_utf8(&word[begin..])
        .ok()?
        .trim_matches(['.', ':']);
    (!path.is_empty() && !STATEMENT_KEYWORDS.contains(&path)).then(|| Name::new(path))
}

/// The members of a function value that call it, or hand it on to be called.
const CALLING_MEMBERS: &[&[u8]] = &[
    b"__call__",
    b"__wrapped__",
    b"__func__",
    b"call",
    b"apply",
    b"bind",
    b"invoke",
    b"callAsFunction",
];

/// The member the occurrence at `at` is the receiver of: `name` in `value.name`.
fn member_of<'t>(text: &'t [u8], at: &Range<u32>) -> Option<&'t [u8]> {
    let rest = text.get(at.end as usize..)?.trim_ascii_start();
    let rest = rest.strip_prefix(b"?").unwrap_or(rest);
    let member = rest.strip_prefix(b".")?.trim_ascii_start();
    let length = member
        .iter()
        .take_while(|byte| byte.is_ascii_alphanumeric() || b"_$".contains(byte))
        .count();
    (length > 0).then(|| &member[..length])
}

/// Whether the occurrence is an item of a collection literal written beside others or alone
/// (`("name", fn, False)`, `[fn]`), no argument of a call: the value is held in the collection.
fn element_of(text: &[u8], at: &Range<u32>) -> bool {
    let after = text
        .get(at.end as usize..)
        .and_then(|rest| rest.iter().find(|byte| !byte.is_ascii_whitespace()));
    byte_before(text, at.start).is_some_and(|byte| b"([{,".contains(&byte))
        && after.is_some_and(|byte| b",)]}".contains(byte))
        && callee_of(text, at).is_none()
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
        // A member read through the value (`get_settings.cache_clear()`) places it as that
        // member's receiver, which calls it only through a member that calls its receiver.
        if member_of(text, at).is_some_and(|member| !CALLING_MEMBERS.contains(&member)) {
            return None;
        }
        let callable = matches!(
            kind,
            DefKind::Function | DefKind::Method | DefKind::Constructor
        );
        let placed = if file.language() == Language::Python && element_of(text, at) {
            (Outcome::Leak(ValuePlace::Element), Basis::ValuePosition)
        } else {
            (Outcome::Candidate(Reason::Unexplained), Basis::Unproven)
        };
        callable.then_some(placed)
    };
    // An action that includes a named template states a call, in no value position.
    let included = file.blob.facts.iter().any(|fact| {
        matches!(fact, Fact::ConfigUse(used)
            if used.of == UseOf::Include
                && used.site.bytes.start <= at.start
                && at.end <= used.site.bytes.end)
    });
    if included {
        return None;
    }
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
    // A shorthand in an object pattern is the read of the member and the name it goes by.
    let shorthand = file.blob.facts.iter().find_map(|fact| match fact {
        Fact::Definition(def)
            if matches!(def.kind, DefKind::Variable | DefKind::Constant)
                && def.name_span.bytes == *at =>
        {
            Some((def.local.name.clone(), declared_in(file, &def.local)))
        }
        Fact::LocalBinding(local)
            if local.kind == LocalKind::Variable && local.site.bytes == *at =>
        {
            Some((local.name.clone(), local.enclosing.clone()))
        }
        _ => None,
    });
    if let Some((name, scope)) = shorthand.filter(|_| PATTERN_FAMILIES.contains(&file.language())) {
        return alias(&name, scope, Basis::Assignment);
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
