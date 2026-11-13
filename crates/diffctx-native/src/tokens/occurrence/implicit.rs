//! The [`Implicit`] kinds of one definition, from its facts and one table.

use crate::decide::Family;
use crate::facts::lang::shell::SCRIPT;
use crate::facts::{Def, EntryKind, Fact, FileFacts, Scope, UseKind};
use crate::tokens::occurrence::Implicit;

/// What `decide` already knows of a definition: `dispatched` is `dispatched_via`,
/// `consumed_outside` is `consumed_outside` of its family.
#[derive(Clone, Copy, Default, Debug)]
pub struct Known {
    pub dispatched: bool,
    pub consumed_outside: bool,
}

/// Where a row looks: the definition's name, its decorators and the words of its
/// signature, or the names written before the member of an `override`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Name,
    Word,
    Base,
}

/// The patterns are words split by spaces; one ending in `*` is a prefix; a `Word`
/// pattern is lower case.
type Row = (Family, Part, Implicit, &'static str);

const TABLE: &[Row] = &[
    (
        Family::Python,
        Part::Name,
        Implicit::Operator,
        "__call__ __getitem__ __setitem__ __delitem__ __getattr__ __setattr__ __delattr__ __getattribute__ __add__ __sub__ __mul__ __matmul__ __truediv__ __floordiv__ __mod__ __pow__ __neg__ __pos__ __invert__ __and__ __or__ __xor__ __lshift__ __rshift__",
    ),
    (Family::Python, Part::Name, Implicit::Protocol, "__*"),
    (Family::Python, Part::Name, Implicit::Test, "test_*"),
    (Family::Python, Part::Word, Implicit::Override, "override"),
    (
        Family::TypeScript,
        Part::Name,
        Implicit::Protocol,
        "toString valueOf toJSON then",
    ),
    (
        Family::TypeScript,
        Part::Word,
        Implicit::Override,
        "override",
    ),
    (
        Family::Rust,
        Part::Base,
        Implicit::Operator,
        "Add Sub Mul Div Rem Neg Not BitAnd BitOr BitXor Shl Shr AddAssign SubAssign MulAssign DivAssign RemAssign Index IndexMut Deref DerefMut Fn FnMut FnOnce",
    ),
    (
        Family::Rust,
        Part::Base,
        Implicit::Protocol,
        "Iterator IntoIterator Display Debug Drop From Into TryFrom FromStr PartialEq Eq PartialOrd Ord Hash Default Clone AsRef Borrow Future",
    ),
    (
        Family::Rust,
        Part::Word,
        Implicit::Outside,
        "no_mangle export_name extern",
    ),
    (Family::Kotlin, Part::Word, Implicit::Operator, "operator"),
    (
        Family::Kotlin,
        Part::Name,
        Implicit::Protocol,
        "iterator next hasNext toString equals hashCode compareTo component* getValue setValue provideDelegate close",
    ),
    (
        Family::Java,
        Part::Name,
        Implicit::Protocol,
        "toString equals hashCode compareTo close iterator next hasNext valueOf readObject writeObject readResolve writeReplace",
    ),
    (
        Family::Go,
        Part::Name,
        Implicit::Protocol,
        "String Error MarshalJSON UnmarshalJSON MarshalText UnmarshalText init",
    ),
    (
        Family::Go,
        Part::Name,
        Implicit::Override,
        "ServeHTTP Read Write Close Seek Less Len Swap Unwrap Is As",
    ),
    (
        Family::Php,
        Part::Name,
        Implicit::Operator,
        "__call __callStatic __get __set __isset __unset __invoke offsetGet offsetSet offsetExists offsetUnset",
    ),
    (
        Family::Php,
        Part::Name,
        Implicit::Protocol,
        "__* count getIterator current key next rewind valid jsonSerialize",
    ),
    (Family::C, Part::Name, Implicit::Operator, "operator*"),
    (Family::C, Part::Name, Implicit::Protocol, "~*"),
    (Family::C, Part::Word, Implicit::Entry, "constructor"),
    (
        Family::Swift,
        Part::Name,
        Implicit::Operator,
        "subscript callAsFunction",
    ),
    (
        Family::Swift,
        Part::Name,
        Implicit::Protocol,
        "description debugDescription deinit makeIterator next",
    ),
    (
        Family::Terraform,
        Part::Word,
        Implicit::Outside,
        "variable output",
    ),
    (Family::Shell, Part::Name, Implicit::Outside, SCRIPT),
    (
        Family::Sql,
        Part::Word,
        Implicit::Outside,
        "trigger event_trigger",
    ),
];

/// The kinds by which `def` of `file` is called without its name written, in the
/// order of [`Implicit::ALL`], each once. Empty means no fact or table row says
/// so, not that nobody does.
pub fn implicit_kinds(file: &FileFacts, def: &Def, known: Known) -> Vec<Implicit> {
    let own = Scope::Def(def.local.clone());
    let mut found = Vec::new();
    if known.dispatched {
        found.push(Implicit::Override);
    }
    if known.consumed_outside {
        found.push(Implicit::Outside);
    }
    let mut tags = vec![(Part::Name, def.local.name.as_str().to_string())];
    let words = def.decorators.iter().map(|d| d.as_str()).chain(
        def.signature
            .iter()
            .flat_map(|s| s.split(|c: char| !c.is_alphanumeric() && c != '_')),
    );
    tags.extend(
        words
            .filter(|word| !word.is_empty())
            .map(|word| (Part::Word, word.to_ascii_lowercase())),
    );
    for fact in &file.blob.facts {
        match fact {
            Fact::EntryPoint(entry) if entry.target == own => found.push(match entry.kind {
                EntryKind::Test => Implicit::Test,
                EntryKind::Main
                | EntryKind::ScriptGuard
                | EntryKind::Command
                | EntryKind::Route
                | EntryKind::Scheduled
                | EntryKind::Tool
                | EntryKind::Fixture => Implicit::Entry,
            }),
            Fact::Reference(r)
                if r.use_kind == UseKind::Override
                    && r.enclosing == own
                    && r.site == def.name_span =>
            {
                found.push(Implicit::Override);
                tags.extend(
                    r.qualifier
                        .iter()
                        .map(|q| (Part::Base, q.as_str().to_string())),
                );
            }
            _ => {}
        }
    }
    for (family, part, kind, patterns) in TABLE {
        let hit = patterns.split_ascii_whitespace().any(|pattern| {
            tags.iter().any(|(at, tag)| {
                at == part
                    && pattern
                        .strip_suffix('*')
                        .map_or(pattern == tag, |prefix| tag.starts_with(prefix))
            })
        });
        if hit && family.has(file.language()) {
            found.push(*kind);
        }
    }
    Implicit::ALL
        .into_iter()
        .filter(|kind| found.contains(kind))
        .collect()
}
