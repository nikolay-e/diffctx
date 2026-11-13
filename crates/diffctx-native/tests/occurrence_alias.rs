//! The alias closure and the value positions of occurrences (`occurrence::alias`),
//! on real sources read by the shipped extractors. Nothing runs diffctx.

use std::ops::Range;
use std::sync::Arc;

use _diffctx::facts::lang::go::GoExtractor;
use _diffctx::facts::lang::python::PythonExtractor;
use _diffctx::facts::lang::rust::RustExtractor;
use _diffctx::facts::lang::typescript::TypeScript;
use _diffctx::facts::{
    DefKind, Extractor, Fact, FileFacts, ModulePath, Name, ProjectId, Scope, UseKind,
};
use _diffctx::resolve::Relation;
use _diffctx::snapshot::{RepoPath, RunBudget};
use _diffctx::tokens::occurrence::{
    Alias, AliasBound, AliasScope, Basis, Case, MAX_ALIAS_HOPS, MAX_ALIAS_NAMES, Outcome, Reason,
    ValuePlace, close_aliases, scan, value_use,
};

type Found = Option<(Outcome, Basis)>;

fn read(extractor: &dyn Extractor, source: &str) -> FileFacts {
    FileFacts {
        path: RepoPath::new("src/sample").expect("a repository path"),
        project: ProjectId::default(),
        module: ModulePath::new("sample"),
        blob: Arc::new(extractor.extract(source.as_bytes(), &RunBudget::default())),
        outdated_grammar: false,
    }
}

fn python(source: &str) -> FileFacts {
    read(&PythonExtractor, source)
}

/// Whether the span is one the facts bind or write: a target, not a use.
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
fn uses(file: &FileFacts, source: &str, name: &str, kind: DefKind) -> Vec<(u32, Found)> {
    let found = scan(file, source.as_bytes(), &[name], Case::of(file.language()));
    found
        .code
        .iter()
        .filter(|at| !is_declaration(file, &at.bytes))
        .map(|at| {
            (
                at.line,
                value_use(file, source.as_bytes(), &at.bytes, kind, None),
            )
        })
        .collect()
}

fn alias(name: &str, scope: AliasScope, basis: Basis) -> Found {
    let alias = Alias {
        name: name.into(),
        scope,
        hops: 1,
    };
    Some((Outcome::Alias(alias), basis))
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

fn leak(place: ValuePlace) -> Found {
    Some((Outcome::Leak(place), Basis::ValuePosition))
}

fn unplaced() -> Found {
    Some((Outcome::Candidate(Reason::Unexplained), Basis::Unproven))
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
        (4, leak(ValuePlace::Argument)),
        (5, leak(ValuePlace::Argument)),
        (6, leak(ValuePlace::Store)),
        (8, unplaced()),
        (9, alias("x", inside(&file, "m"), Basis::Assignment)),
        (10, unplaced()),
        (11, None),
        (13, alias("p", inside(&file, "C"), Basis::Assignment)),
        (15, leak(ValuePlace::Store)),
    ];
    assert_eq!(found, expected);
}

#[test]
fn a_plain_read_hedges_a_callable_only() {
    let source = "X = 1\ndef m():\n    return X\n";
    let file = python(source);
    let read_of = |kind| uses(&file, source, "X", kind).remove(0).1;
    assert_eq!(read_of(DefKind::Constant), None);
    assert_eq!(read_of(DefKind::Function), unplaced());
}

#[test]
fn an_import_or_a_re_export_is_no_value_position() {
    let source = "from m import f\n";
    let file = python(source);
    let found = scan(&file, source.as_bytes(), &["f"], Case::Sensitive);
    let at = &found.code[0].bytes;
    let bytes = source.as_bytes();
    for relation in [Relation::Imports, Relation::Reexports] {
        assert_eq!(
            value_use(&file, bytes, at, DefKind::Function, Some(relation)),
            None
        );
    }
    let unbound = value_use(&file, bytes, at, DefKind::Function, None);
    assert_eq!(unbound, unplaced(), "no reference, no relation");
}

#[test]
fn a_shape_the_bytes_do_not_prove_stays_unplaced() {
    for source in [
        "def f():\n    pass\na = b = f\n",
        "def f():\n    pass\ng: object = f\n",
        "def f():\n    pass\ng = f or h\n",
        "def f():\n    pass\ng, h = f, 1\n",
    ] {
        let file = python(source);
        let found = uses(&file, source, "f", DefKind::Function);
        assert_eq!(found, vec![(3, unplaced())], "{source}");
    }
}

#[test]
fn a_type_alias_names_its_target_in_the_declaring_scope() {
    let source = "export class F {}\nexport type G = F;\n";
    let file = read(&TypeScript, source);
    let found = uses(&file, source, "F", DefKind::Class);
    assert_eq!(found, vec![(2, alias("G", module(), Basis::TypeAlias))]);
}

#[test]
fn a_typescript_assignment_names_the_definition() {
    let source = "export function f() {}\nexport const g = f;\n";
    let file = read(&TypeScript, source);
    let found = uses(&file, source, "f", DefKind::Function);
    assert_eq!(found, vec![(2, alias("g", module(), Basis::Assignment))]);
}

#[test]
fn a_name_given_to_a_call_is_an_argument_where_the_family_says_so() {
    let go = "package p\n\nfunc F() {}\n\nfunc main() {\n\tApply(F)\n}\n";
    let rust = "fn f() {}\nfn main() {\n    run(f);\n}\n";
    let file = read(&GoExtractor, go);
    let found = uses(&file, go, "F", DefKind::Function);
    assert_eq!(found, vec![(6, leak(ValuePlace::Argument))]);
    let file = read(&RustExtractor, rust);
    let found = uses(&file, rust, "f", DefKind::Function);
    assert_eq!(found, vec![(3, leak(ValuePlace::Argument))]);
}

/// The step of the closure is the scan of one name and the reading of its
/// occurrences.
fn closure_of(source: &str) -> (Vec<Alias>, Option<AliasBound>) {
    let file = python(source);
    close_aliases(&[Name::from("f")], |name, _scope| {
        let bytes = source.as_bytes();
        let found = scan(&file, bytes, &[name.as_str()], Case::Sensitive);
        found
            .code
            .iter()
            .filter(|at| !is_declaration(&file, &at.bytes))
            .filter_map(
                |at| match value_use(&file, bytes, &at.bytes, DefKind::Function, None) {
                    Some((Outcome::Alias(alias), _)) => Some(alias),
                    _ => None,
                },
            )
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

fn fan(names: usize) -> String {
    let mut source = String::from("def f():\n    pass\n");
    for name in 0..names {
        source.push_str(&format!("a{name} = f\n"));
    }
    source
}

fn named(aliases: &[Alias]) -> Vec<(&str, usize)> {
    aliases
        .iter()
        .map(|alias| (alias.name.as_str(), alias.hops))
        .collect()
}

#[test]
fn a_chain_of_the_bound_hops_closes_and_one_more_is_partial() {
    let (aliases, bound) = closure_of(&chain(MAX_ALIAS_HOPS));
    assert_eq!(named(&aliases), vec![("g1", 1), ("g2", 2), ("g3", 3)]);
    assert_eq!(bound, None);
    let (aliases, bound) = closure_of(&chain(MAX_ALIAS_HOPS + 1));
    assert_eq!(aliases.len(), MAX_ALIAS_HOPS + 1);
    let past = MAX_ALIAS_HOPS + 1;
    assert_eq!(
        bound,
        Some(AliasBound {
            hops: past,
            names: past
        })
    );
}

#[test]
fn the_most_names_close_and_one_more_is_partial() {
    let (aliases, bound) = closure_of(&fan(MAX_ALIAS_NAMES));
    assert_eq!((aliases.len(), bound), (MAX_ALIAS_NAMES, None));
    let (_, bound) = closure_of(&fan(MAX_ALIAS_NAMES + 1));
    let names = MAX_ALIAS_NAMES + 1;
    assert_eq!(bound, Some(AliasBound { hops: 1, names }));
}

#[test]
fn the_closure_takes_each_step_in_name_order_and_ends_on_a_cycle() {
    let source = "def f():\n    pass\nb = f\na = f\nc = a\na = c\n";
    let (aliases, bound) = closure_of(source);
    assert_eq!(named(&aliases), vec![("a", 1), ("b", 1), ("c", 2)]);
    assert_eq!(bound, None, "a name met again is no new name");
}
