//! The alias closure and the value positions of occurrences (`occurrence::alias`),
//! on real sources read by the shipped extractors: each alias row and each leak
//! place today's facts express, the bound, and what stays unplaced. Nothing runs
//! diffctx.

use std::ops::Range;
use std::sync::Arc;

use _diffctx::facts::lang::go::GoExtractor;
use _diffctx::facts::lang::python::PythonExtractor;
use _diffctx::facts::lang::rust::RustExtractor;
use _diffctx::facts::lang::typescript::TypeScript;
use _diffctx::facts::{
    BlobFacts, DefKind, Extractor, Fact, FileFacts, Lexical, ModulePath, Name, ProjectId, Scope,
    UseKind,
};
use _diffctx::occurrence::{
    Alias, AliasBound, AliasScope, Basis, Case, MAX_ALIAS_HOPS, MAX_ALIAS_NAMES, Outcome, Reason,
    ValuePlace, ValueUse, close_aliases, scan, value_use,
};
use _diffctx::resolve::Relation;
use _diffctx::snapshot::{RepoPath, RunBudget};

fn file_of(blob: BlobFacts) -> FileFacts {
    FileFacts {
        path: RepoPath::new("src/sample").expect("a repository path"),
        project: ProjectId::default(),
        module: ModulePath::new("sample"),
        blob: Arc::new(blob),
        outdated_grammar: false,
    }
}

fn read(extractor: &dyn Extractor, source: &str) -> FileFacts {
    file_of(extractor.extract(source.as_bytes(), &RunBudget::default()))
}

fn python(source: &str) -> FileFacts {
    read(&PythonExtractor, source)
}

/// Whether the span is one the facts bind or write: the target of an
/// assignment, not an occurrence of a use.
fn is_declaration(file: &FileFacts, bytes: &Range<u32>) -> bool {
    file.blob.facts.iter().any(|fact| match fact {
        Fact::Definition(def) => def.name_span.bytes == *bytes,
        Fact::LocalBinding(local) => local.site.bytes == *bytes,
        Fact::Reference(write) => write.use_kind == UseKind::Write && write.site.bytes == *bytes,
        _ => false,
    })
}

/// The value use of each code occurrence of `name` that is not a declaration,
/// with its line.
fn uses(file: &FileFacts, source: &str, name: &str, kind: DefKind) -> Vec<(u32, ValueUse)> {
    let found = scan(file, source.as_bytes(), &[name], Case::of(file.language()));
    found
        .code
        .iter()
        .filter(|at| !is_declaration(file, &at.bytes))
        .map(|at| {
            let value = value_use(file, source.as_bytes(), &at.bytes, kind, None);
            (at.line, value)
        })
        .collect()
}

fn alias(name: &str, scope: AliasScope, basis: Basis) -> ValueUse {
    ValueUse::Alias(
        Alias {
            name: name.into(),
            scope,
            hops: 1,
        },
        basis,
    )
}

fn module() -> AliasScope {
    AliasScope::Declared(Scope::Module)
}

fn inside(file: &FileFacts, definition: &str) -> AliasScope {
    let def = file
        .defs()
        .find(|def| def.local.name.as_str() == definition)
        .expect("a definition of that name");
    AliasScope::Declared(Scope::Def(def.local.clone()))
}

const PLACES: &str = "def f():
    pass
g = f
h(f)
h(k=f)
obj.attr = f
def m():
    return f
    x = f
    y = [f]
    z = f()
class C:
    p = f
    def __init__(self):
        self.q = f
";

#[test]
fn each_value_position_the_python_facts_tell_is_named() {
    let file = python(PLACES);
    let found = uses(&file, PLACES, "f", DefKind::Function);
    let expected = vec![
        (3, alias("g", module(), Basis::Assignment)),
        (4, ValueUse::Leak(ValuePlace::Argument)),
        (5, ValueUse::Leak(ValuePlace::Argument)),
        (6, ValueUse::Leak(ValuePlace::Store)),
        (8, ValueUse::Unplaced),
        (9, alias("x", inside(&file, "m"), Basis::Assignment)),
        (10, ValueUse::Unplaced),
        (11, ValueUse::Plain),
        (13, alias("p", inside(&file, "C"), Basis::Assignment)),
        (15, ValueUse::Leak(ValuePlace::Store)),
    ];
    assert_eq!(found, expected);
}

#[test]
fn an_assignment_to_a_plain_name_is_an_alias_and_not_a_leak() {
    let source = "def f():\n    pass\ng = f\n";
    let file = python(source);
    let found = uses(&file, source, "f", DefKind::Function);
    assert_eq!(found, vec![(3, alias("g", module(), Basis::Assignment))]);
    let (outcome, basis) = found[0].1.clone().outcome().expect("an outcome");
    assert_eq!(outcome.as_str(), "alias");
    assert_eq!(basis, Basis::Assignment);
}

#[test]
fn a_use_no_fact_places_is_a_candidate_and_never_ours() {
    let source = "def f():\n    pass\ndef m():\n    return f\n";
    let file = python(source);
    let (_, unplaced) = uses(&file, source, "f", DefKind::Function).remove(0);
    assert_eq!(unplaced, ValueUse::Unplaced);
    assert_eq!(
        unplaced.outcome(),
        Some((Outcome::Candidate(Reason::Unexplained), Basis::Unproven))
    );
    assert_eq!(ValueUse::Plain.outcome(), None);
    assert_eq!(
        ValueUse::Leak(ValuePlace::Return).outcome(),
        Some((Outcome::Leak(ValuePlace::Return), Basis::ValuePosition))
    );
}

#[test]
fn a_plain_read_hedges_a_callable_only() {
    let source = "X = 1\ndef m():\n    return X\n";
    let file = python(source);
    let read_of = |kind| uses(&file, source, "X", kind).remove(0).1;
    assert_eq!(read_of(DefKind::Constant), ValueUse::Plain);
    assert_eq!(read_of(DefKind::Function), ValueUse::Unplaced);
}

#[test]
fn an_import_or_a_re_export_is_no_value_position() {
    let source = "from m import f\n";
    let file = python(source);
    let found = scan(&file, source.as_bytes(), &["f"], Case::Sensitive);
    let at = &found.code[0].bytes;
    let bytes = source.as_bytes();
    for relation in [Relation::Imports, Relation::Reexports] {
        let value = value_use(&file, bytes, at, DefKind::Function, Some(relation));
        assert_eq!(value, ValueUse::Plain);
    }
    let unbound = value_use(&file, bytes, at, DefKind::Function, None);
    assert_eq!(unbound, ValueUse::Unplaced, "no reference, no relation");
}

#[test]
fn a_shape_the_bytes_do_not_prove_stays_unplaced() {
    for (source, line) in [
        ("def f():\n    pass\na = b = f\n", 3),
        ("def f():\n    pass\ng: object = f\n", 3),
        ("def f():\n    pass\ng = f or h\n", 3),
        ("def f():\n    pass\ng, h = f, 1\n", 3),
    ] {
        let file = python(source);
        let found = uses(&file, source, "f", DefKind::Function);
        assert_eq!(found, vec![(line, ValueUse::Unplaced)], "{source}");
    }
}

#[test]
fn a_comment_after_the_value_ends_the_statement() {
    let source = "def f():\n    pass\ng = f  # kept\n";
    let comment = source.find("# kept").expect("the comment") as u32;
    let plain = python(source);
    assert_eq!(
        uses(&plain, source, "f", DefKind::Function),
        vec![(3, ValueUse::Unplaced)],
        "without ranges the comment is code"
    );
    let mut blob = PythonExtractor.extract(source.as_bytes(), &RunBudget::default());
    blob.lexical = vec![(comment..source.len() as u32 - 1, Lexical::Comment)];
    blob.in_source_order();
    let ranged = file_of(blob);
    assert_eq!(
        uses(&ranged, source, "f", DefKind::Function),
        vec![(3, alias("g", module(), Basis::Assignment))]
    );
}

#[test]
fn a_type_alias_names_its_target_in_the_declaring_scope() {
    let source = "export class F {}\nexport type G = F;\n";
    let file = read(&TypeScript, source);
    let found = uses(&file, source, "F", DefKind::Class);
    assert_eq!(found, vec![(2, alias("G", module(), Basis::TypeAlias))]);
}

#[test]
fn a_typescript_assignment_and_a_destructuring_name_the_definition() {
    let source = "export function f() {}\nexport const g = f;\nconst { f: h } = obj;\n";
    let file = read(&TypeScript, source);
    let found = uses(&file, source, "f", DefKind::Function);
    assert_eq!(
        found,
        vec![
            (2, alias("g", module(), Basis::Assignment)),
            (3, alias("h", module(), Basis::Destructuring)),
        ]
    );
}

#[test]
fn a_name_given_to_a_call_is_an_argument_where_the_family_says_so() {
    let go = "package p\n\nfunc F() {}\n\nfunc main() {\n\tApply(F)\n}\n";
    let rust = "fn f() {}\nfn main() {\n    run(f);\n}\n";
    let argument = ValueUse::Leak(ValuePlace::Argument);
    let file = read(&GoExtractor, go);
    assert_eq!(
        uses(&file, go, "F", DefKind::Function),
        vec![(6, argument.clone())]
    );
    let file = read(&RustExtractor, rust);
    assert_eq!(
        uses(&file, rust, "f", DefKind::Function),
        vec![(3, argument)]
    );
}

/// Each assignment of `name` in `source` is an alias of `f`: the step of the
/// closure is the scan of one name and the reading of its occurrences.
fn closure_of(source: &str) -> _diffctx::occurrence::AliasClosure {
    let file = python(source);
    close_aliases(&[Name::from("f")], |name, _scope| {
        let found = scan(&file, source.as_bytes(), &[name.as_str()], Case::Sensitive);
        found
            .code
            .iter()
            .filter(|at| !is_declaration(&file, &at.bytes))
            .filter_map(|at| {
                match value_use(&file, source.as_bytes(), &at.bytes, DefKind::Function, None) {
                    ValueUse::Alias(alias, _) => Some(alias),
                    _ => None,
                }
            })
            .collect()
    })
}

fn chain(length: usize) -> String {
    let mut source = String::from("def f():\n    pass\ng1 = f\n");
    for hop in 2..=length {
        source.push_str(&format!("g{hop} = g{}\n", hop - 1));
    }
    source
}

#[test]
fn a_chain_of_the_bound_hops_closes() {
    assert_eq!(MAX_ALIAS_HOPS, 3);
    let closure = closure_of(&chain(MAX_ALIAS_HOPS));
    let found: Vec<(&str, usize)> = closure
        .aliases
        .iter()
        .map(|alias| (alias.name.as_str(), alias.hops))
        .collect();
    assert_eq!(found, vec![("g1", 1), ("g2", 2), ("g3", 3)]);
    assert_eq!(closure.bound, None);
}

#[test]
fn a_chain_one_hop_past_the_bound_is_partial() {
    let closure = closure_of(&chain(MAX_ALIAS_HOPS + 1));
    assert_eq!(closure.aliases.len(), MAX_ALIAS_HOPS + 1);
    assert_eq!(
        closure.bound,
        Some(AliasBound {
            hops: MAX_ALIAS_HOPS + 1,
            names: MAX_ALIAS_HOPS + 1
        })
    );
}

fn fan(names: usize) -> String {
    let mut source = String::from("def f():\n    pass\n");
    for name in 0..names {
        source.push_str(&format!("a{name} = f\n"));
    }
    source
}

#[test]
fn the_most_names_close_and_one_more_is_partial() {
    assert_eq!(MAX_ALIAS_NAMES, 16);
    let closed = closure_of(&fan(MAX_ALIAS_NAMES));
    assert_eq!(closed.aliases.len(), MAX_ALIAS_NAMES);
    assert_eq!(closed.bound, None);
    let past = closure_of(&fan(MAX_ALIAS_NAMES + 1));
    assert_eq!(
        past.bound,
        Some(AliasBound {
            hops: 1,
            names: MAX_ALIAS_NAMES + 1
        })
    );
}

#[test]
fn the_closure_takes_each_step_in_name_order_and_ends_on_a_cycle() {
    let source = "def f():\n    pass\nb = f\na = f\nc = a\na = c\n";
    let closure = closure_of(source);
    let found: Vec<(&str, usize)> = closure
        .aliases
        .iter()
        .map(|alias| (alias.name.as_str(), alias.hops))
        .collect();
    assert_eq!(found, vec![("a", 1), ("b", 1), ("c", 2)]);
    assert_eq!(closure.bound, None, "a name met again is no new name");
    assert_eq!(closure, closure_of(source), "one answer for one input");
}
