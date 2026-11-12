//! A name a language lets one quote is recorded in one spelling in every fact a
//! declaration or a use makes: Kotlin and Swift backticks, Rust raw identifiers
//! (`r#match` is the identifier `match`, and `r#spelled` is `spelled` where the
//! word is no keyword), SQL double quotes. A caller written quoted and a
//! definition written bare, or the reverse, are one name to the language, so a
//! fact that keeps the quotes joins to nothing and leaves a false absence (fj#173).
//!
//! Each family has a seed that declares quoted names, uses them quoted and,
//! where the language allows, bare, and binds a quoted parameter or variable. Two
//! properties are read off its facts:
//!
//! - what a declaration and a use name: every definition and every use is
//!   recorded under the bare name, and no such name keeps a quote;
//! - what else a fact holds: a local, an import, an export, the chain written
//!   before a member, the type a receiver has, and the site of a use, which reads
//!   as the name it records.
//!
//! Java has no quoting syntax, so it has no seed. Other families that quote
//! something (a TypeScript member written as a string, a Terraform label) are
//! not identifiers a language lets one write either way.

use _diffctx::facts::lang::kotlin::KotlinExtractor;
use _diffctx::facts::lang::rust::RustExtractor;
use _diffctx::facts::lang::sql::SqlExtractor;
use _diffctx::facts::lang::swift::SwiftExtractor;
use _diffctx::facts::{
    BlobFacts, Disambig, Export, Extractor, Fact, LocalKey, Receiver, Ref, Scope,
};
use _diffctx::snapshot::RunBudget;

struct Seed {
    family: &'static str,
    source: &'static str,
    extract: fn(&str) -> BlobFacts,
    /// Whether a recorded name still carries the family's quoting.
    quoted: fn(&str) -> bool,
    /// The names the source defines, each recorded once bare.
    defined: &'static [&'static str],
    /// The names the source uses, each with at least this many references.
    used: &'static [(&'static str, usize)],
    /// The names a local, a parameter or an import binds.
    bound: &'static [&'static str],
    /// The names written before a member (`a.b()`), each in a reference's qualifier.
    qualifiers: &'static [&'static str],
    /// The quote a use is written in, when its site must read as its name.
    site_quote: Option<char>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    Definition,
    Use,
    Local,
    Import,
    Export,
    Qualifier,
    Receiver,
}

const KOTLIN: Seed = Seed {
    family: "kotlin",
    source: "package spell

import lib.`odd import`
import lib.Tool as `tool alias`

class `Spaced Type` {
    fun `member fn`(): Int = 1
}

fun `top fn`(): Int = 1

fun `with param`(`my arg`: `Spaced Type`): Int = `my arg`.`member fn`()

fun user(): Int {
    val `my local` = `Spaced Type`()
    `odd import`()
    return `top fn`() + `my local`.`member fn`()
}
",
    extract: kotlin,
    quoted: backticked,
    defined: &["Spaced Type", "member fn", "top fn", "with param", "user"],
    used: &[
        ("Spaced Type", 2),
        ("member fn", 2),
        ("top fn", 1),
        ("odd import", 1),
    ],
    bound: &["my arg", "my local", "odd import", "tool alias"],
    qualifiers: &["my arg", "my local"],
    site_quote: Some('`'),
};

const SWIFT: Seed = Seed {
    family: "swift",
    source: "func `default`() -> Int { 1 }

final class Box {
    func `repeat`() -> Int { 2 }
}

func use(_ `case`: Box) -> Int {
    let `in` = Box()
    return `default`() + `case`.`repeat`() + `in`.`repeat`()
}
",
    extract: swift,
    quoted: backticked,
    defined: &["default", "repeat", "Box", "use"],
    used: &[("default", 1), ("repeat", 2), ("Box", 2)],
    bound: &["case", "in"],
    qualifiers: &["case", "in"],
    site_quote: Some('`'),
};

// `r#spelled` is `spelled` too, so a bare call of it is legal; `match`, `try`,
// `loop`, `async` and `await` are keywords that only a raw identifier names.
const RUST: Seed = Seed {
    family: "rust",
    source: r##"fn r#spelled() -> u32 { 1 }
fn r#match() -> u32 { 2 }

struct Holder;

impl Holder {
    fn r#try(&self) -> u32 { 3 }
}

mod r#async {
    pub fn r#await() -> u32 { 4 }
}

pub use self::r#async::r#await;

pub fn user(r#in: &Holder) -> u32 {
    let (r#left, r#right) = pair();
    let r#loop = r#spelled();
    let r#kept = spelled();
    r#loop + r#kept + r#match() + r#in.r#try() + r#left.r#try() + r#right
}
"##,
    extract: rust,
    quoted: raw,
    defined: &["spelled", "match", "Holder", "try", "await", "user"],
    used: &[("spelled", 2), ("match", 1), ("try", 2)],
    bound: &["in", "left", "right", "loop", "kept"],
    // A local of a known type is the receiver's type, so only one of no known
    // type is written before the member.
    qualifiers: &["left"],
    site_quote: None,
};

// An unquoted name folds to lower case and a quoted one keeps its case, so
// `"plain_fn"` and `PLAIN_FN` are the bare `plain_fn`.
const SQL: Seed = Seed {
    family: "sql",
    source: r#"CREATE FUNCTION "my schema"."my fn"() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$;
CREATE FUNCTION plain_fn() RETURNS int LANGUAGE sql AS $$ SELECT "my schema"."my fn"() $$;
CREATE VIEW "My View" AS SELECT 1;
SELECT "my schema"."my fn"(), "plain_fn"(), PLAIN_FN();
SELECT * FROM "My View";
"#,
    extract: sql,
    quoted: double_quoted,
    defined: &["my schema", "my fn", "plain_fn", "My View"],
    used: &[("my fn", 2), ("plain_fn", 2), ("My View", 1)],
    bound: &[],
    qualifiers: &["my schema"],
    site_quote: Some('"'),
};

fn kotlin(source: &str) -> BlobFacts {
    KotlinExtractor.extract(source.as_bytes(), &RunBudget::default())
}

fn swift(source: &str) -> BlobFacts {
    SwiftExtractor.extract(source.as_bytes(), &RunBudget::default())
}

fn rust(source: &str) -> BlobFacts {
    RustExtractor.extract(source.as_bytes(), &RunBudget::default())
}

fn sql(source: &str) -> BlobFacts {
    SqlExtractor.extract(source.as_bytes(), &RunBudget::default())
}

fn backticked(name: &str) -> bool {
    name.contains('`')
}

fn raw(name: &str) -> bool {
    name.split("::").any(|segment| segment.starts_with("r#"))
}

fn double_quoted(name: &str) -> bool {
    name.contains('"')
}

fn key_names(key: &LocalKey, out: &mut Vec<(Role, String)>) {
    out.extend(
        key.container
            .iter()
            .chain(std::iter::once(&key.name))
            .map(|name| (Role::Definition, name.to_string())),
    );
    if let Some(Disambig::ImplOf(receiver)) = &key.disambig {
        out.push((Role::Definition, receiver.to_string()));
    }
}

fn scope_names(scope: &Scope, out: &mut Vec<(Role, String)>) {
    match scope {
        Scope::Def(key) => key_names(key, out),
        Scope::Callback { callee, parent, .. } => {
            out.extend(callee.iter().map(|name| (Role::Use, name.to_string())));
            scope_names(parent, out);
        }
        Scope::Module => {}
    }
}

/// Every name a fact of the blob holds, with the part it plays.
fn recorded(blob: &BlobFacts) -> Vec<(Role, String)> {
    let mut out: Vec<(Role, String)> = Vec::new();
    for fact in &blob.facts {
        match fact {
            Fact::Definition(def) => {
                key_names(&def.local, &mut out);
                out.extend(
                    def.decorators
                        .iter()
                        .map(|name| (Role::Definition, name.to_string())),
                );
            }
            Fact::Reference(reference) => {
                out.push((Role::Use, reference.name.to_string()));
                out.extend(
                    reference
                        .qualifier
                        .iter()
                        .map(|name| (Role::Qualifier, name.to_string())),
                );
                match &reference.receiver {
                    Receiver::Typed(ty) | Receiver::New(ty) => {
                        out.push((Role::Receiver, ty.to_string()));
                    }
                    Receiver::Module(module) => out.push((Role::Receiver, module.to_string())),
                    Receiver::CallResult(names) => {
                        out.extend(names.iter().map(|name| (Role::Receiver, name.to_string())));
                    }
                    Receiver::None | Receiver::SelfType | Receiver::Unknown => {}
                }
                scope_names(&reference.enclosing, &mut out);
            }
            Fact::Import(import) => {
                out.push((Role::Import, import.module.to_string()));
                for (name, alias) in &import.names {
                    out.push((Role::Import, name.to_string()));
                    out.extend(alias.iter().map(|a| (Role::Import, a.to_string())));
                }
                out.extend(import.default.iter().map(|d| (Role::Import, d.to_string())));
                scope_names(&import.enclosing, &mut out);
            }
            Fact::Export(Export::Named {
                name, alias, from, ..
            }) => {
                out.push((Role::Export, name.to_string()));
                out.extend(alias.iter().map(|a| (Role::Export, a.to_string())));
                out.extend(from.iter().map(|m| (Role::Export, m.to_string())));
            }
            Fact::Export(Export::Star { from, alias, .. }) => {
                out.push((Role::Export, from.to_string()));
                out.extend(alias.iter().map(|a| (Role::Export, a.to_string())));
            }
            Fact::Export(Export::Default { local, .. }) => {
                out.extend(local.iter().map(|l| (Role::Export, l.to_string())));
            }
            Fact::LocalBinding(local) => {
                out.push((Role::Local, local.name.to_string()));
                scope_names(&local.enclosing, &mut out);
            }
            Fact::EntryPoint(entry) => scope_names(&entry.target, &mut out),
            _ => {}
        }
    }
    out
}

fn references(blob: &BlobFacts) -> impl Iterator<Item = &Ref> {
    blob.facts.iter().filter_map(|fact| match fact {
        Fact::Reference(reference) => Some(reference),
        _ => None,
    })
}

fn read(seed: &Seed) -> BlobFacts {
    let blob = (seed.extract)(seed.source);
    assert!(
        blob.losses.is_empty(),
        "{}: the seed does not read whole: {:?}",
        seed.family,
        blob.losses
    );
    blob
}

fn assert_unquoted(seed: &Seed, names: &[(Role, String)], roles: &[Role]) {
    let kept: Vec<&(Role, String)> = names
        .iter()
        .filter(|(role, name)| roles.contains(role) && (seed.quoted)(name))
        .collect();
    assert!(
        kept.is_empty(),
        "{}: a name keeps its quoting: {kept:?}",
        seed.family
    );
}

fn holds(names: &[(Role, String)], roles: &[Role], wanted: &str) -> bool {
    names
        .iter()
        .any(|(role, name)| roles.contains(role) && name == wanted)
}

/// A definition and each use of it are one name, bare.
fn a_use_is_spelled_as_its_declaration(seed: &Seed) {
    let blob = read(seed);
    let names = recorded(&blob);
    for wanted in seed.defined {
        assert!(
            holds(&names, &[Role::Definition], wanted),
            "{}: no definition of `{wanted}`; defined: {:?}",
            seed.family,
            names
                .iter()
                .filter(|(role, _)| *role == Role::Definition)
                .map(|(_, name)| name)
                .collect::<Vec<_>>()
        );
    }
    for (wanted, least) in seed.used {
        let found = references(&blob)
            .filter(|reference| reference.name.as_str() == *wanted)
            .count();
        assert!(
            found >= *least,
            "{}: `{wanted}` is used {found} time(s) under that name, the seed uses it {least}; \
             used: {:?}",
            seed.family,
            references(&blob)
                .map(|reference| reference.name.as_str())
                .collect::<Vec<_>>()
        );
    }
    assert_unquoted(seed, &names, &[Role::Definition, Role::Use]);
}

/// A binding, a chain, a receiver's type and a use's site spell the name the
/// same way.
fn the_rest_of_a_fact_is_spelled_the_same_way(seed: &Seed) {
    let blob = read(seed);
    let names = recorded(&blob);
    for wanted in seed.bound {
        assert!(
            holds(&names, &[Role::Local, Role::Import], wanted),
            "{}: nothing binds `{wanted}`",
            seed.family
        );
    }
    for wanted in seed.qualifiers {
        assert!(
            holds(&names, &[Role::Qualifier], wanted),
            "{}: no reference is reached through `{wanted}`; qualifiers: {:?}",
            seed.family,
            names
                .iter()
                .filter(|(role, _)| *role == Role::Qualifier)
                .map(|(_, name)| name)
                .collect::<Vec<_>>()
        );
    }
    assert_unquoted(
        seed,
        &names,
        &[
            Role::Local,
            Role::Import,
            Role::Export,
            Role::Qualifier,
            Role::Receiver,
        ],
    );
    let Some(quote) = seed.site_quote else {
        return;
    };
    for reference in references(&blob) {
        let range = reference.site.bytes.start as usize..reference.site.bytes.end as usize;
        let written = seed
            .source
            .get(range)
            .unwrap_or_else(|| panic!("{}: a site off the source: {reference:?}", seed.family));
        let bare = written
            .strip_prefix(quote)
            .and_then(|inner| inner.strip_suffix(quote))
            .unwrap_or(written);
        assert!(
            bare.eq_ignore_ascii_case(reference.name.as_str()),
            "{}: the site of `{}` reads `{written}`",
            seed.family,
            reference.name
        );
    }
}

#[test]
fn kotlin_a_use_is_spelled_as_its_declaration() {
    a_use_is_spelled_as_its_declaration(&KOTLIN);
}

#[test]
fn kotlin_the_rest_of_a_fact_is_spelled_the_same_way() {
    the_rest_of_a_fact_is_spelled_the_same_way(&KOTLIN);
}

#[test]
fn swift_a_use_is_spelled_as_its_declaration() {
    a_use_is_spelled_as_its_declaration(&SWIFT);
}

#[test]
fn swift_the_rest_of_a_fact_is_spelled_the_same_way() {
    the_rest_of_a_fact_is_spelled_the_same_way(&SWIFT);
}

#[test]
fn rust_a_use_is_spelled_as_its_declaration() {
    a_use_is_spelled_as_its_declaration(&RUST);
}

#[test]
fn rust_the_rest_of_a_fact_is_spelled_the_same_way() {
    the_rest_of_a_fact_is_spelled_the_same_way(&RUST);
}

#[test]
fn sql_a_use_is_spelled_as_its_declaration() {
    a_use_is_spelled_as_its_declaration(&SQL);
}

#[test]
fn sql_the_rest_of_a_fact_is_spelled_the_same_way() {
    the_rest_of_a_fact_is_spelled_the_same_way(&SQL);
}
