//! The [`Implicit`] kinds of one definition, from its facts and one table.

use std::path::Path;

use crate::tokens::occurrence::Implicit;
use crate::vocabulary::family::Family;
use diffctx_base::testfiles::is_test_path;
use diffctx_facts::facts::lang::shell::SHEBANG;
use diffctx_facts::facts::{Def, EntryKind, Fact, FileFacts, Scope, UseKind};

/// What `decide` already knows of a definition: `dispatched` is `dispatched_via`,
/// `consumed_outside` is `consumed_outside` of its family, `root_module` that its
/// Terraform directory is entered from outside, applied or called by another
/// repository (`root_module` of the resolver).
#[derive(Clone, Copy, Default, Debug)]
pub struct Known {
    pub dispatched: bool,
    pub consumed_outside: bool,
    pub root_module: bool,
    /// Another language reaches it by name (`exported` of its [`View`]).
    pub exported: bool,
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
/// pattern is lower case; one starting with `!` takes a name out of the row.
type Row = (Family, Part, Implicit, &'static str);

/// The decorators the C extractor gives a function another language calls by name
/// (`extern "C"` linkage, an `__asm__` label). A prototype that carries one declares
/// the function wherever its definition is.
pub(super) const C_EXPORTS: &str = "extern_c asm_label";

const TABLE: &[Row] = &[
    (
        Family::Python,
        Part::Name,
        Implicit::Operator,
        "__call__ __getitem__ __setitem__ __delitem__ __getattr__ __setattr__ __delattr__ __getattribute__ __add__ __sub__ __mul__ __matmul__ __truediv__ __floordiv__ __mod__ __pow__ __neg__ __pos__ __invert__ __and__ __or__ __xor__ __lshift__ __rshift__",
    ),
    (
        Family::Python,
        Part::Name,
        Implicit::Protocol,
        "__* !__init__ !__new__",
    ),
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
        "no_mangle export_name extern wasm_bindgen pyfunction pymethods pyclass napi",
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
    (Family::C, Part::Word, Implicit::Outside, C_EXPORTS),
    (
        Family::Swift,
        Part::Name,
        Implicit::Operator,
        "subscript callAsFunction ~= buildBlock buildExpression buildOptional buildEither buildArray buildLimitedAvailability buildFinalResult buildPartialBlock",
    ),
    (
        Family::Swift,
        Part::Name,
        Implicit::Protocol,
        "description debugDescription deinit makeIterator next hash hashValue",
    ),
    (
        Family::Terraform,
        Part::Word,
        Implicit::Outside,
        "variable output",
    ),
    (Family::Shell, Part::Word, Implicit::Outside, SHEBANG),
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
    if known.consumed_outside || known.exported {
        found.push(Implicit::Outside);
    }
    let mut tags = vec![(Part::Name, def.local.name.as_str().to_string())];
    let words = def
        .decorators
        .iter()
        .flat_map(|d| d.as_str().split("::"))
        .chain(
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
                | EntryKind::Fixture
                | EntryKind::Migration => Implicit::Entry,
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
    // A root module is set from outside; a module a call sources is set by its
    // caller. Which is which the repository's calls say, not the depth of the path
    // or the files a root keeps (fj#193).
    let rooted = known.root_module;
    for (family, part, kind, patterns) in TABLE {
        let names = |pattern: &str| {
            tags.iter().any(|(at, tag)| {
                at == part
                    && pattern
                        .strip_suffix('*')
                        .map_or(pattern == tag, |prefix| tag.starts_with(prefix))
            })
        };
        let hit = patterns
            .split_ascii_whitespace()
            .any(|pattern| pattern.strip_prefix('!').is_none() && names(pattern))
            && !patterns
                .split_ascii_whitespace()
                .filter_map(|pattern| pattern.strip_prefix('!'))
                .any(names);
        if hit && family.has(file.language()) && (rooted || *family != Family::Terraform) {
            found.push(*kind);
        }
    }
    // The runner is the entry the answer names for a test file's test and any Rust `#[test]`.
    let run_by_runner =
        is_test_path(Path::new(file.path.as_str())) || Family::Rust.has(file.language());
    Implicit::ALL
        .into_iter()
        .filter(|kind| found.contains(kind) && !(*kind == Implicit::Test && run_by_runner))
        .collect()
}

/// Whether the facts state who enters `def`, as the answer's `entered_by` names it:
/// `main`, a script guard or an entry a framework makes. That entry is no hedge.
pub fn entered(file: &FileFacts, def: &Def) -> bool {
    let own = Scope::Def(def.local.clone());
    file.blob.facts.iter().any(|fact| match fact {
        Fact::EntryPoint(entry) => {
            entry.target == own
                && match entry.kind {
                    EntryKind::Test => false,
                    EntryKind::Main | EntryKind::ScriptGuard => true,
                    _ => entry.framework.is_some(),
                }
        }
        _ => false,
    })
}
