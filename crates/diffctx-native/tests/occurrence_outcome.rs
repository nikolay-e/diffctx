//! The outcome of each occurrence (`occurrence::outcomes`) on real Python files
//! committed to a real repository and read by the real resolver: each row of the
//! contract's two tables that today's facts can produce, the `f(g(f))` trap, a
//! `not_ours` whose basis the accounting cannot verify, and the base side. The
//! expectations are read off the sources below, never off a run of diffctx.

mod snapshot_fixture;

use std::sync::Arc;

use _diffctx::facts::{DefKind, FactsCache, FileFacts, Lexical, facts_of};
use _diffctx::occurrence::{
    Basis, Entry, FileOutcomes, Outcome, Reason, View, admitted, counts_against, outcomes,
};
use _diffctx::resolve::{
    Binding, CResolver, CandidateReason, Definition, Evidence, EvidenceKind, GoResolver,
    JavaResolver, KotlinResolver, PhpResolver, PythonResolver, Relation, Resolution, RustResolver,
    ShellResolver, TypeScriptResolver, UnresolvedReason,
};
use _diffctx::snapshot::{AnalysisContext, RangeSpec, RepoPath, RunBudget, Side, SideTag};
use snapshot_fixture::Repo;

struct World {
    _repo: Repo,
    files: Vec<FileFacts>,
    bindings: Vec<Binding>,
    definitions: Vec<Definition>,
    /// The definitions of the base side of the range.
    base_definitions: Vec<Definition>,
    texts: Vec<(String, String)>,
}

fn repo_path(name: &str) -> RepoPath {
    RepoPath::new(name).expect("a repository path")
}

/// `base` committed, then `head` written over it and committed: the files of the
/// head side are the union, `head` winning. An empty `base` is one snapshot.
#[derive(Clone, Copy)]
enum Lang {
    Python,
    TypeScript,
    Java,
    Kotlin,
    Rust,
    Go,
    Php,
    C,
    Shell,
}

/// What the language's resolver says of one side.
fn resolved(
    lang: Lang,
    ctx: &AnalysisContext,
    side: SideTag,
    cache: &FactsCache,
) -> (Vec<Binding>, Vec<Definition>) {
    match lang {
        Lang::Python => {
            let it = PythonResolver::open(ctx, side, cache).expect("the side resolves");
            (it.bindings().to_vec(), it.definitions().to_vec())
        }
        Lang::TypeScript => {
            let it = TypeScriptResolver::open(ctx, side, cache).expect("the side resolves");
            (it.bindings().to_vec(), it.definitions().to_vec())
        }
        Lang::Java => {
            let it = JavaResolver::open(ctx, side, cache).expect("the side resolves");
            (it.bindings().to_vec(), it.definitions().to_vec())
        }
        Lang::Kotlin => {
            let it = KotlinResolver::open(ctx, side, cache).expect("the side resolves");
            (it.bindings().to_vec(), it.definitions().to_vec())
        }
        Lang::Rust => {
            let it = RustResolver::open(ctx, side, cache).expect("the side resolves");
            (it.bindings().to_vec(), it.definitions().to_vec())
        }
        Lang::Go => {
            let it = GoResolver::open(ctx, side, cache).expect("the side resolves");
            (it.bindings().to_vec(), it.definitions().to_vec())
        }
        Lang::Php => {
            let it = PhpResolver::open(ctx, side, cache).expect("the side resolves");
            (it.bindings().to_vec(), it.definitions().to_vec())
        }
        Lang::C => {
            let it = CResolver::open(ctx, side, cache).expect("the side resolves");
            (it.bindings().to_vec(), it.definitions().to_vec())
        }
        Lang::Shell => {
            let it = ShellResolver::open(ctx, side, cache).expect("the side resolves");
            (it.bindings().to_vec(), it.definitions().to_vec())
        }
    }
}

fn build(base: &[(&str, &str)], head: &[(&str, &str)]) -> World {
    build_in(Lang::Python, base, head)
}

fn build_in(lang: Lang, base: &[(&str, &str)], head: &[(&str, &str)]) -> World {
    let repo = Repo::init();
    let mut texts: Vec<(String, String)> = Vec::new();
    let put = |texts: &mut Vec<(String, String)>, path: &str, text: &str| {
        repo.write(path, text);
        match texts.iter_mut().find(|(known, _)| known == path) {
            Some(slot) => slot.1 = text.to_string(),
            None => texts.push((path.to_string(), text.to_string())),
        }
    };
    for (path, text) in base {
        put(&mut texts, path, text);
    }
    let range = if base.is_empty() {
        for (path, text) in head {
            put(&mut texts, path, text);
        }
        RangeSpec::At(Side::Tree(repo.commit("snapshot")))
    } else {
        let first = repo.commit("base");
        for (path, text) in head {
            put(&mut texts, path, text);
        }
        RangeSpec::Review {
            base: first,
            head: repo.commit("head"),
        }
    };
    let ctx = AnalysisContext::new(repo.root.clone(), range, RunBudget::default())
        .expect("an analysis context");
    let cache = FactsCache::disabled();
    let (bindings, definitions) = resolved(lang, &ctx, SideTag::Head, &cache);
    let (_, base_definitions) = resolved(lang, &ctx, SideTag::Base, &cache);
    let files = texts
        .iter()
        .map(|(path, _)| {
            facts_of(&ctx, SideTag::Head, &cache, &repo_path(path))
                .expect("the facts of a file")
                .expect("a source file")
        })
        .collect();
    World {
        bindings,
        definitions,
        base_definitions,
        files,
        texts,
        _repo: repo,
    }
}

fn snapshot(files: &[(&str, &str)]) -> World {
    build(&[], files)
}

fn snapshot_in(lang: Lang, files: &[(&str, &str)]) -> World {
    build_in(lang, &[], files)
}

impl World {
    fn text(&self, file: &str) -> &str {
        &self
            .texts
            .iter()
            .find(|(path, _)| path == file)
            .expect("a file of the snapshot")
            .1
    }

    fn facts(&self, file: &str) -> &FileFacts {
        self.files
            .iter()
            .find(|facts| facts.path.as_str() == file)
            .expect("the facts of a file of the snapshot")
    }

    /// The definition `Owner.name` (or `name`) of `file`.
    fn def(&self, file: &str, dotted: &str) -> &Definition {
        find(&self.definitions, file, dotted)
    }

    fn outcomes(&self, subject: &Definition, file: &str, names: &[&str]) -> FileOutcomes {
        let view = View::new(&self.files, &self.bindings, &self.definitions, true);
        outcomes(
            &view,
            self.facts(file),
            self.text(file).as_bytes(),
            subject,
            names,
        )
    }
}

fn find<'a>(definitions: &'a [Definition], file: &str, dotted: &str) -> &'a Definition {
    let mut parts: Vec<&str> = dotted.split('.').collect();
    let name = parts.pop().expect("a name");
    definitions
        .iter()
        .find(|def| {
            def.file.as_str() == file
                && def.key.local.name.as_str() == name
                && def
                    .key
                    .local
                    .container
                    .iter()
                    .map(|part| part.as_str())
                    .eq(parts.iter().copied())
        })
        .unwrap_or_else(|| panic!("no definition {dotted} in {file}"))
}

/// The definition of `file` declared at `line`: overloads share a name.
fn declared_at<'a>(definitions: &'a [Definition], file: &str, line: u32) -> &'a Definition {
    definitions
        .iter()
        .find(|def| def.file.as_str() == file && def.span.lines.0 == line)
        .unwrap_or_else(|| panic!("no definition at {file}:{line}"))
}

/// The one entry on `line`.
fn on(found: &FileOutcomes, line: u32) -> &Entry {
    let here: Vec<&Entry> = found
        .entries
        .iter()
        .filter(|entry| entry.occurrence.line == line)
        .collect();
    assert_eq!(here.len(), 1, "line {line}: {found:#?}");
    here[0]
}

fn shape(entry: &Entry) -> (&'static str, &'static str, Option<Relation>) {
    (entry.outcome.as_str(), entry.basis.as_str(), entry.relation)
}

const LIB_F: &str = "def f():\n    return 1\n";

#[test]
fn a_call_bound_to_the_definition_is_ours_and_an_import_without_name_spans_stays_a_candidate() {
    let world = snapshot(&[
        ("lib.py", LIB_F),
        (
            "app.py",
            "from lib import f\n\n\ndef main():\n    return f()\n",
        ),
    ]);
    let found = world.outcomes(world.def("lib.py", "f"), "app.py", &["f"]);
    assert_eq!(found.entries.len(), 2, "{found:#?}");
    assert!(!found.lost);
    assert_eq!(found.mentions, 0);
    let import = on(&found, 1);
    assert_eq!(import.outcome, Outcome::Candidate(Reason::WideBinding));
    assert_eq!(import.relation, Some(Relation::Imports));
    assert_eq!(import.basis, Basis::Unproven);
    assert_eq!(
        shape(on(&found, 5)),
        ("ours", "resolved", Some(Relation::Calls))
    );
}

#[test]
fn a_definition_name_is_a_declaration_the_definitions_own_or_another() {
    let world = snapshot(&[("lib.py", LIB_F), ("other.py", "def f(y):\n    return y\n")]);
    let subject = world.def("lib.py", "f");
    let own = world.outcomes(subject, "lib.py", &["f"]);
    assert_eq!(shape(on(&own, 1)), ("declaration", "definition_name", None));
    let other = world.outcomes(subject, "other.py", &["f"]);
    assert_eq!(
        shape(on(&other, 1)),
        ("declaration", "definition_name", None)
    );
}

#[test]
fn a_parameter_is_a_declaration_and_a_use_of_it_is_not_ours_by_its_scope() {
    let world = snapshot(&[
        ("lib.py", LIB_F),
        ("app.py", "def g(f):\n    return f(1)\n"),
    ]);
    let found = world.outcomes(world.def("lib.py", "f"), "app.py", &["f"]);
    assert_eq!(shape(on(&found, 1)), ("declaration", "local_site", None));
    assert_eq!(
        shape(on(&found, 2)),
        ("not_ours", "local", Some(Relation::Calls))
    );
}

#[test]
fn a_local_variable_site_is_the_declaration_of_another_binding() {
    let world = snapshot(&[
        ("lib.py", LIB_F),
        ("app.py", "def g():\n    f = 2\n    return f\n"),
    ]);
    let found = world.outcomes(world.def("lib.py", "f"), "app.py", &["f"]);
    assert_eq!(
        shape(on(&found, 2)),
        ("declaration", "local_site", None),
        "{found:#?}"
    );
    assert_eq!(on(&found, 3).outcome, Outcome::NotOurs);
}

#[test]
fn a_nested_def_site_is_the_declaration_of_another_binding() {
    let world = snapshot(&[
        ("lib.py", LIB_F),
        (
            "app.py",
            "def g():\n    def f():\n        return 2\n    return f\n",
        ),
    ]);
    let found = world.outcomes(world.def("lib.py", "f"), "app.py", &["f"]);
    assert_eq!(
        shape(on(&found, 2)),
        ("declaration", "definition_name", None),
        "{found:#?}"
    );
}

#[test]
fn a_typescript_closure_site_is_the_declaration_of_another_binding() {
    let world = snapshot_in(
        Lang::TypeScript,
        &[
            ("lib.ts", "export function f() {\n  return 1;\n}\n"),
            (
                "app.ts",
                "export function g() {\n  const f = () => 2;\n  return 1;\n}\n",
            ),
        ],
    );
    let found = world.outcomes(world.def("lib.ts", "f"), "app.ts", &["f"]);
    assert_eq!(
        shape(on(&found, 2)),
        ("declaration", "local_site", None),
        "{found:#?}"
    );
}

#[test]
fn a_value_a_name_is_written_through_keeps_its_hedge_at_a_local_site() {
    let world = snapshot(&[(
        "lib.py",
        "counter = 0\n\n\ndef bump():\n    global counter\n    counter = 5\n",
    )]);
    let found = world.outcomes(world.def("lib.py", "counter"), "lib.py", &["counter"]);
    let write = on(&found, 6);
    assert!(
        matches!(write.outcome, Outcome::Ours | Outcome::Candidate(_)),
        "a write to the module's variable is no declaration of a local: {found:#?}"
    );
}

#[test]
fn a_resolution_to_another_definition_of_the_side_is_not_ours() {
    let world = snapshot(&[
        ("lib.py", LIB_F),
        (
            "app.py",
            "def f():\n    return 2\n\n\ndef main():\n    return f()\n",
        ),
    ]);
    let found = world.outcomes(world.def("lib.py", "f"), "app.py", &["f"]);
    assert_eq!(
        shape(on(&found, 1)),
        ("declaration", "definition_name", None)
    );
    assert_eq!(
        shape(on(&found, 6)),
        ("not_ours", "other_definition", Some(Relation::Calls))
    );
}

const OVERRIDES: &str = "class Base:
    def run(self):
        return 1

    def go(self):
        return self.run()


class Child(Base):
    def run(self):
        return 2
";

#[test]
fn an_override_is_ours_by_its_relation_and_the_overriders_beside_a_call_do_not_hedge_it() {
    let world = snapshot(&[("base.py", OVERRIDES)]);
    let found = world.outcomes(world.def("base.py", "Base.run"), "base.py", &["run"]);
    assert_eq!(
        shape(on(&found, 2)),
        ("declaration", "definition_name", None)
    );
    assert_eq!(
        shape(on(&found, 6)),
        ("ours", "resolved", Some(Relation::Calls))
    );
    assert_eq!(
        shape(on(&found, 10)),
        ("ours", "resolved", Some(Relation::Overrides)),
        "the override's name is the declaration of another definition and a use of this one"
    );
}

#[test]
fn a_call_that_lands_in_the_override_family_is_a_dispatch_candidate() {
    let world = snapshot(&[("base.py", OVERRIDES)]);
    let found = world.outcomes(world.def("base.py", "Child.run"), "base.py", &["run"]);
    assert_eq!(
        shape(on(&found, 2)),
        ("declaration", "definition_name", None)
    );
    assert_eq!(
        shape(on(&found, 10)),
        ("declaration", "definition_name", None)
    );
    let call = on(&found, 6);
    assert_eq!(call.relation, Some(Relation::Calls));
    assert!(
        matches!(
            call.outcome,
            Outcome::Candidate(Reason::Dispatch | Reason::Resolver(CandidateReason::Dispatch))
        ),
        "{call:#?}"
    );
}

#[test]
fn a_candidate_resolution_stays_a_candidate_with_the_resolvers_reason() {
    let world = snapshot(&[(
        "shapes.py",
        "class Square:
    def area(self):
        return 1


class Circle:
    def area(self):
        return 2


def total(shape):
    return shape.area()
",
    )]);
    let found = world.outcomes(
        world.def("shapes.py", "Square.area"),
        "shapes.py",
        &["area"],
    );
    assert_eq!(
        shape(on(&found, 2)),
        ("declaration", "definition_name", None)
    );
    assert_eq!(
        shape(on(&found, 7)),
        ("declaration", "definition_name", None)
    );
    let call = on(&found, 12);
    assert_eq!(
        call.outcome,
        Outcome::Candidate(Reason::Resolver(CandidateReason::UntypedReceiver))
    );
    assert_eq!(call.relation, Some(Relation::Calls));
}

#[test]
fn a_language_name_nothing_shadows_is_not_ours() {
    let world = snapshot(&[
        ("lib.py", "def print(x):\n    return x\n"),
        ("app.py", "def say():\n    print(\"a\")\n"),
    ]);
    let found = world.outcomes(world.def("lib.py", "print"), "app.py", &["print"]);
    assert_eq!(
        shape(on(&found, 2)),
        ("not_ours", "builtin", Some(Relation::Calls))
    );
}

#[test]
fn a_name_an_import_binds_to_a_module_outside_the_repository_is_not_ours() {
    let world = snapshot(&[
        ("lib.py", "def join(a):\n    return a\n"),
        (
            "app.py",
            "from os.path import join\n\n\ndef here():\n    return join(\"a\")\n",
        ),
    ]);
    let found = world.outcomes(world.def("lib.py", "join"), "app.py", &["join"]);
    assert_eq!(
        shape(on(&found, 5)),
        ("not_ours", "external_module", Some(Relation::Calls))
    );
}

#[test]
fn an_external_call_no_member_of_the_name_can_be_is_not_ours() {
    let world = snapshot(&[
        ("lib.py", "def join(a):\n    return a\n"),
        (
            "app.py",
            "import os\n\n\ndef here():\n    return os.join(\"a\")\n",
        ),
    ]);
    let found = world.outcomes(world.def("lib.py", "join"), "app.py", &["join"]);
    assert_eq!(
        shape(on(&found, 5)),
        ("not_ours", "external_name", Some(Relation::Calls)),
        "no import binds `join` and no class has a member of that name"
    );
}

#[test]
fn an_external_call_a_member_of_the_name_could_be_stays_a_candidate() {
    let world = snapshot(&[
        ("lib.py", "def join(a):\n    return a\n"),
        (
            "path.py",
            "class Path:\n    def join(self, a):\n        return a\n",
        ),
        (
            "app.py",
            "import os\n\n\ndef here():\n    return os.join(\"a\")\n",
        ),
    ]);
    let found = world.outcomes(world.def("lib.py", "join"), "app.py", &["join"]);
    let call = on(&found, 5);
    assert_eq!(
        call.outcome,
        Outcome::Candidate(Reason::BasisUnverified),
        "`Path.join` is a member of that name: the head of the chain is not carried"
    );
    assert_eq!(call.relation, Some(Relation::Calls));
}

#[test]
fn f_of_g_of_f_binds_each_f_by_its_own_span() {
    let world = snapshot(&[
        (
            "lib.py",
            "def f(x):\n    return x\n\n\ndef g(x):\n    return x\n",
        ),
        (
            "app.py",
            "from lib import f, g\n\n\ndef main():\n    return f(g(f))\n",
        ),
    ]);
    let found = world.outcomes(world.def("lib.py", "f"), "app.py", &["f"]);
    let text = world.text("app.py");
    let outer = text.find("f(g(f))").expect("the nested call") as u32;
    let inner = outer + 4;
    let nested: Vec<&Entry> = found
        .entries
        .iter()
        .filter(|entry| entry.occurrence.line == 5)
        .collect();
    assert_eq!(nested.len(), 2, "{found:#?}");
    assert_eq!(nested[0].occurrence.bytes, outer..outer + 1);
    assert_eq!(nested[1].occurrence.bytes, inner..inner + 1);
    assert_eq!(
        shape(nested[0]),
        ("ours", "resolved", Some(Relation::Calls)),
        "the outer call"
    );
    assert_eq!(
        shape(nested[1]),
        ("ours", "resolved", Some(Relation::PassesAsValue)),
        "the inner f is passed, not called: the outer call's binding does not hold it"
    );
}

#[test]
fn a_spelling_no_binding_names_is_unexplained() {
    let world = snapshot(&[
        ("lib.py", LIB_F),
        ("app.py", "def h(g):\n    return g(f=1)\n"),
    ]);
    let found = world.outcomes(world.def("lib.py", "f"), "app.py", &["f"]);
    let member = on(&found, 2);
    assert_eq!(member.outcome, Outcome::Candidate(Reason::Unexplained));
    assert_eq!(member.relation, None);
    assert_eq!(member.basis, Basis::Unproven);
}

const CONFIG: &str = "def config():\n    return {}\n";

#[test]
fn a_bare_name_heading_a_chain_is_the_local_of_that_name() {
    let world = snapshot(&[
        ("lib.py", CONFIG),
        (
            "app.py",
            "def g(load):\n    config = load()\n    return config.get(\"a\")\n",
        ),
    ]);
    let found = world.outcomes(world.def("lib.py", "config"), "app.py", &["config"]);
    assert_eq!(
        shape(on(&found, 3)),
        ("not_ours", "local", None),
        "{found:#?}"
    );
}

#[test]
fn a_member_spelled_like_a_local_is_no_bare_name() {
    let world = snapshot(&[
        ("lib.py", CONFIG),
        (
            "app.py",
            "def g(a):\n    config = 2\n    return a.config.get(\"a\")\n",
        ),
    ]);
    let found = world.outcomes(world.def("lib.py", "config"), "app.py", &["config"]);
    assert_eq!(
        on(&found, 3).outcome,
        Outcome::Candidate(Reason::Unresolved(UnresolvedReason::UntypedRead)),
        "the middle of a chain is an attribute of another object: {found:#?}"
    );
}

#[test]
fn a_bare_name_heading_a_chain_is_what_an_import_binds_it_to_elsewhere() {
    let world = snapshot(&[
        ("lib.py", CONFIG),
        ("other.py", CONFIG),
        (
            "app.py",
            "from other import config\n\n\ndef g():\n    return config.get(\"a\")\n",
        ),
    ]);
    let found = world.outcomes(world.def("lib.py", "config"), "app.py", &["config"]);
    assert_eq!(
        shape(on(&found, 5)),
        ("not_ours", "other_definition", Some(Relation::Reads)),
        "{found:#?}"
    );
}

#[test]
fn a_bare_name_heading_a_chain_stays_a_candidate_where_the_import_binds_the_subject() {
    let world = snapshot(&[
        (
            "lib.py",
            "class Foo:\n    @staticmethod\n    def bar():\n        return 1\n",
        ),
        (
            "app.py",
            "from lib import Foo\n\n\ndef g():\n    return Foo.bar()\n",
        ),
    ]);
    let found = world.outcomes(world.def("lib.py", "Foo"), "app.py", &["Foo"]);
    let head = on(&found, 5);
    assert!(
        matches!(head.outcome, Outcome::Ours | Outcome::Candidate(_)),
        "the import binds the name to the subject: {found:#?}"
    );
}

#[test]
fn a_bare_name_heading_a_chain_is_no_member_of_a_class_it_is_written_outside_of() {
    let world = snapshot(&[
        (
            "lib.py",
            "class Runner:\n    def run(self):\n        return 1\n",
        ),
        ("app.py", "def g(items):\n    return run.x(items)\n"),
    ]);
    let subject = world.def("lib.py", "Runner.run");
    let found = world.outcomes(subject, "app.py", &["run"]);
    assert_eq!(
        shape(on(&found, 2)),
        ("not_ours", "bare_name", None),
        "{found:#?}"
    );
}

#[test]
fn a_bare_name_heading_a_chain_in_the_class_body_stays_a_candidate() {
    let world = snapshot(&[(
        "lib.py",
        "class Runner:\n    def run(self):\n        return 1\n\n    doc = run.__doc__\n",
    )]);
    let found = world.outcomes(world.def("lib.py", "Runner.run"), "lib.py", &["run"]);
    let head = on(&found, 5);
    assert!(
        matches!(head.outcome, Outcome::Ours | Outcome::Candidate(_)),
        "inside the class a bare name is the member: {found:#?}"
    );
}

#[test]
fn a_binding_to_a_module_is_ours_for_its_own_module_or_one_that_offers_it_on() {
    let own = snapshot(&[
        ("util.py", "def util():\n    return 1\n"),
        (
            "app.py",
            "import util\n\n\ndef use():\n    return repr(util)\n",
        ),
    ]);
    let found = own.outcomes(own.def("util.py", "util"), "app.py", &["util"]);
    assert_eq!(
        shape(on(&found, 5)),
        ("ours", "own_module", Some(Relation::PassesAsValue))
    );

    let offered = snapshot(&[
        ("pkg/__init__.py", "from .m import thing\n"),
        ("pkg/m.py", "def thing():\n    return 1\n"),
        (
            "app.py",
            "import pkg\n\n\ndef use():\n    return repr(pkg)\n",
        ),
    ]);
    let found = offered.outcomes(offered.def("pkg/m.py", "thing"), "app.py", &["pkg"]);
    assert_eq!(
        shape(on(&found, 5)),
        ("ours", "own_module", Some(Relation::PassesAsValue))
    );
}

#[test]
fn a_binding_to_another_module_is_not_ours_when_its_facts_are_whole() {
    let world = snapshot(&[
        ("util.py", "X = 1\n"),
        ("other.py", "def util():\n    return 2\n"),
        (
            "app.py",
            "import util\n\n\ndef use():\n    return repr(util)\n",
        ),
    ]);
    let found = world.outcomes(world.def("other.py", "util"), "app.py", &["util"]);
    assert_eq!(
        shape(on(&found, 5)),
        ("not_ours", "other_module", Some(Relation::PassesAsValue))
    );
}

#[test]
fn two_bindings_that_disagree_on_one_span_are_a_candidate() {
    let mut world = snapshot(&[
        ("lib.py", LIB_F),
        (
            "app.py",
            "def f():\n    return 2\n\n\ndef main():\n    return f()\n",
        ),
    ]);
    let subject = world.def("lib.py", "f").clone();
    let call = world
        .bindings
        .iter()
        .find(|binding| binding.file.as_str() == "app.py" && binding.site.lines.0 == 6)
        .expect("the call's binding")
        .clone();
    let mut claim = call;
    claim.resolution = Resolution::Resolved {
        target: subject.key.clone(),
        evidence: Evidence {
            kind: EvidenceKind::SameScope,
            through: Vec::new(),
        },
    };
    world.bindings.push(claim);
    let found = world.outcomes(&subject, "app.py", &["f"]);
    assert_eq!(on(&found, 6).outcome, Outcome::Candidate(Reason::Conflict));
}

#[test]
fn a_removed_definition_is_accounted_in_the_head_files() {
    let world = build(
        &[
            ("lib.py", LIB_F),
            (
                "app.py",
                "from lib import f\n\n\ndef main():\n    return f()\n",
            ),
        ],
        &[
            ("lib.py", "def other():\n    return 1\n"),
            (
                "app2.py",
                "def f():\n    return 2\n\n\ndef run():\n    return f()\n",
            ),
        ],
    );
    let removed = find(&world.base_definitions, "lib.py", "f");
    let stranded = world.outcomes(removed, "app.py", &["f"]);
    assert_eq!(
        on(&stranded, 5).outcome,
        Outcome::Candidate(Reason::Unresolved(UnresolvedReason::NoBinding)),
        "a head use bound to nothing may have broken"
    );
    assert_eq!(on(&stranded, 5).relation, Some(Relation::Calls));
    let elsewhere = world.outcomes(removed, "app2.py", &["f"]);
    assert_eq!(
        shape(on(&elsewhere, 6)),
        ("not_ours", "other_definition", Some(Relation::Calls)),
        "a head use bound to another head definition is not ours"
    );
}

#[test]
fn a_file_read_with_a_loss_makes_every_spelling_a_candidate() {
    let world = snapshot(&[
        ("lib.py", LIB_F),
        (
            "app.py",
            "from lib import f\n\n\ndef main():\n    return f()\n",
        ),
    ]);
    let facts = world.facts("app.py");
    let mut blob = (*facts.blob).clone();
    blob.parse_error(vec![(1, 1)]);
    let lossy = FileFacts {
        blob: Arc::new(blob),
        ..facts.clone()
    };
    let view = View::new(&world.files, &world.bindings, &world.definitions, true);
    let found = outcomes(
        &view,
        &lossy,
        world.text("app.py").as_bytes(),
        world.def("lib.py", "f"),
        &["f"],
    );
    assert!(found.lost);
    assert_eq!(found.entries.len(), 2);
    for entry in &found.entries {
        assert_eq!(entry.outcome, Outcome::Candidate(Reason::FileLost));
        assert_eq!(entry.relation, None);
    }
}

#[test]
fn a_mention_nobody_explained_is_counted_not_listed() {
    let world = snapshot(&[
        ("lib.py", LIB_F),
        ("app.py", "def main():\n    return \"f\"\n"),
    ]);
    let facts = world.facts("app.py");
    let text = world.text("app.py");
    let quote = text.find("\"f\"").expect("the literal") as u32;
    let mut blob = (*facts.blob).clone();
    blob.lexical = vec![(quote..quote + 3, Lexical::String)];
    blob.in_source_order();
    let read = FileFacts {
        blob: Arc::new(blob),
        ..facts.clone()
    };
    let view = View::new(&world.files, &world.bindings, &world.definitions, true);
    let found = outcomes(
        &view,
        &read,
        text.as_bytes(),
        world.def("lib.py", "f"),
        &["f"],
    );
    assert!(found.entries.is_empty(), "{found:#?}");
    assert_eq!(found.mentions, 1);
    assert!(!found.lost);
}

#[test]
fn an_unbound_spelling_counts_against_the_relations_the_kind_admits() {
    let world = snapshot(&[
        ("lib.py", LIB_F),
        ("app.py", "def h(g):\n    return g(f=1)\n"),
    ]);
    let found = world.outcomes(world.def("lib.py", "f"), "app.py", &["f"]);
    let unbound = on(&found, 2);
    assert!(counts_against(unbound, DefKind::Function, Relation::Calls));
    assert!(counts_against(
        unbound,
        DefKind::Function,
        Relation::PassesAsValue
    ));
    assert!(!counts_against(unbound, DefKind::Function, Relation::Reads));
    assert!(counts_against(unbound, DefKind::Variable, Relation::Reads));
    for kind in [DefKind::Function, DefKind::Class, DefKind::Variable] {
        assert!(!admitted(kind).is_empty());
    }
    let imported = Entry {
        relation: Some(Relation::Imports),
        ..unbound.clone()
    };
    assert!(counts_against(
        &imported,
        DefKind::Function,
        Relation::Imports
    ));
    assert!(!counts_against(
        &imported,
        DefKind::Function,
        Relation::Calls
    ));
}

#[test]
fn a_segment_of_an_import_module_path_is_no_reference() {
    let world = snapshot(&[
        ("pricing.py", "def pricing():\n    return 1\n"),
        ("app.py", "from pricing import total\n"),
    ]);
    let found = world.outcomes(world.def("pricing.py", "pricing"), "app.py", &["pricing"]);
    assert_eq!(
        shape(on(&found, 1)),
        ("not_ours", "other_module", None),
        "{found:#?}"
    );
}

#[test]
fn the_name_an_import_takes_from_a_module_of_its_own_name_stays_a_reference() {
    let world = snapshot(&[
        ("pricing.py", "def pricing():\n    return 1\n"),
        ("app.py", "from pricing import (\n    pricing,\n)\n"),
    ]);
    let found = world.outcomes(world.def("pricing.py", "pricing"), "app.py", &["pricing"]);
    assert_eq!(
        shape(on(&found, 1)),
        ("not_ours", "other_module", None),
        "{found:#?}"
    );
    let name = on(&found, 2);
    assert!(
        matches!(name.outcome, Outcome::Ours | Outcome::Candidate(_)),
        "the imported name is the subject: {found:#?}"
    );
}

const OVER: &str = "package o;\n\nclass Over {\n    int add(int a) {\n        return a;\n    }\n\n    int add(long a) {\n        return 0;\n    }\n\n    int add(int a, int b) {\n        return a + b;\n    }\n\n    void use() {\n        add(1);\n    }\n}\n";

const OVERLOADED: Outcome = Outcome::Candidate(Reason::Resolver(CandidateReason::Overloaded));

#[test]
fn an_overload_the_argument_count_leaves_out_of_the_candidates_is_not_called() {
    let world = snapshot_in(Lang::Java, &[("o/Over.java", OVER)]);
    let subject = declared_at(&world.definitions, "o/Over.java", 12);
    let found = world.outcomes(subject, "o/Over.java", &["add"]);
    assert_eq!(
        shape(on(&found, 17)),
        ("not_ours", "other_definition", Some(Relation::Calls)),
        "{found:#?}"
    );
}

#[test]
fn an_overload_among_the_candidates_stays_a_candidate() {
    let world = snapshot_in(Lang::Java, &[("o/Over.java", OVER)]);
    let subject = declared_at(&world.definitions, "o/Over.java", 4);
    let found = world.outcomes(subject, "o/Over.java", &["add"]);
    assert_eq!(on(&found, 17).outcome, OVERLOADED, "{found:#?}");
}

#[test]
fn an_overload_of_a_base_class_stays_a_candidate() {
    let world = snapshot_in(
        Lang::Java,
        &[(
            "o/Over.java",
            "package o;\n\nclass Base {\n    int add(int a, int b) {\n        return a + b;\n    }\n}\n\nclass Over extends Base {\n    int add(int a) {\n        return a;\n    }\n\n    int add(long a) {\n        return 0;\n    }\n\n    void use() {\n        add(1);\n    }\n}\n",
        )],
    );
    let subject = declared_at(&world.definitions, "o/Over.java", 4);
    let found = world.outcomes(subject, "o/Over.java", &["add"]);
    assert_eq!(on(&found, 19).outcome, OVERLOADED, "{found:#?}");
}

#[test]
fn a_removed_overload_stays_a_candidate_where_its_neighbours_are() {
    let head = OVER.replace(
        "    int add(int a, int b) {\n        return a + b;\n    }\n\n",
        "",
    );
    let world = build_in(
        Lang::Java,
        &[("o/Over.java", OVER)],
        &[("o/Over.java", head.as_str())],
    );
    let removed = declared_at(&world.base_definitions, "o/Over.java", 12);
    let found = world.outcomes(removed, "o/Over.java", &["add"]);
    assert_eq!(on(&found, 13).outcome, OVERLOADED, "{found:#?}");
}

const DISP: &str = "package d;\n\ninterface Repo {\n    String find(int id);\n\n    String find(int id, int page);\n}\n\nclass Mem implements Repo {\n    @Override public String find(int id) {\n        return \"m\";\n    }\n\n    public String find(int id, int page) {\n        return \"p\";\n    }\n}\n\nclass Use {\n    String through(Repo repo) {\n        return repo.find(1);\n    }\n}\n";

#[test]
fn a_call_through_a_supertype_is_not_to_an_overload_of_another_arity() {
    let world = snapshot_in(Lang::Java, &[("d/Disp.java", DISP)]);
    let subject = declared_at(&world.definitions, "d/Disp.java", 6);
    let found = world.outcomes(subject, "d/Disp.java", &["find"]);
    assert_eq!(
        shape(on(&found, 21)),
        ("not_ours", "other_definition", Some(Relation::Calls)),
        "{found:#?}"
    );
}

#[test]
fn a_call_through_a_supertype_stays_a_candidate_on_an_override() {
    let world = snapshot_in(Lang::Java, &[("d/Disp.java", DISP)]);
    let subject = declared_at(&world.definitions, "d/Disp.java", 10);
    let found = world.outcomes(subject, "d/Disp.java", &["find"]);
    assert_eq!(
        on(&found, 21).outcome,
        Outcome::Candidate(Reason::Resolver(CandidateReason::Dispatch)),
        "{found:#?}"
    );
}

const CLEANUP: &str = "#!/bin/bash\ncleanup() {\n  rm -f \"$1\"\n}\ncleanup now\necho cleanup\nsudo cleanup\ntrap cleanup EXIT\n";

#[test]
fn a_word_after_a_command_that_names_no_function_is_no_call() {
    let world = snapshot_in(Lang::Shell, &[("run.sh", CLEANUP)]);
    let found = world.outcomes(world.def("run.sh", "cleanup"), "run.sh", &["cleanup"]);
    assert_eq!(
        shape(on(&found, 6)),
        ("not_ours", "builtin", None),
        "{found:#?}"
    );
    assert_eq!(
        shape(on(&found, 7)),
        ("not_ours", "builtin", None),
        "{found:#?}"
    );
}

#[test]
fn a_function_name_inside_a_longer_word_is_no_call() {
    let world = snapshot_in(
        Lang::Shell,
        &[(
            "run.sh",
            "#!/bin/bash\ncleanup() {\n  rm -f \"$1\"\n}\nbash cleanup.sh\nmake cleanup-all\ntar --cleanup x\ntrap cleanup EXIT\n",
        )],
    );
    let found = world.outcomes(world.def("run.sh", "cleanup"), "run.sh", &["cleanup"]);
    for line in [5, 6, 7] {
        assert_eq!(
            shape(on(&found, line)),
            ("not_ours", "builtin", None),
            "line {line}: {found:#?}"
        );
    }
    assert_eq!(
        on(&found, 8).outcome,
        Outcome::Candidate(Reason::Unexplained),
        "`trap cleanup EXIT` names it whole: {found:#?}"
    );
}

#[test]
fn a_word_after_a_command_that_may_run_a_function_stays_a_candidate() {
    let world = snapshot_in(Lang::Shell, &[("run.sh", CLEANUP)]);
    let found = world.outcomes(world.def("run.sh", "cleanup"), "run.sh", &["cleanup"]);
    assert_eq!(
        on(&found, 8).outcome,
        Outcome::Candidate(Reason::Unexplained),
        "`trap cleanup EXIT` calls it: {found:#?}"
    );
    assert_eq!(
        shape(on(&found, 5)),
        ("ours", "resolved", Some(Relation::Calls))
    );
}

#[test]
fn a_scripts_own_name_after_a_plain_command_is_no_run_unless_the_command_runs_it() {
    let world = snapshot_in(
        Lang::Shell,
        &[(
            "scripts/check.sh",
            "#!/bin/sh -e\n\nset -x\ncat check\nsudo check\n",
        )],
    );
    let script = world.def("scripts/check.sh", "<script>");
    let found = world.outcomes(script, "scripts/check.sh", &["check"]);
    assert_eq!(
        shape(on(&found, 4)),
        ("not_ours", "builtin", None),
        "{found:#?}"
    );
    assert_eq!(
        on(&found, 5).outcome,
        Outcome::Candidate(Reason::Unexplained),
        "`sudo check` runs a program of that name: {found:#?}"
    );
}

#[test]
fn a_scripts_name_after_a_command_the_table_does_not_list_stays_a_candidate() {
    let world = snapshot_in(
        Lang::Shell,
        &[(
            "scripts/deploy.sh",
            "#!/bin/sh -e\n\nbash deploy\npython deploy\n",
        )],
    );
    let script = world.def("scripts/deploy.sh", "<script>");
    let found = world.outcomes(script, "scripts/deploy.sh", &["deploy"]);
    for line in [3, 4] {
        assert_eq!(
            on(&found, line).outcome,
            Outcome::Candidate(Reason::Unexplained),
            "line {line}: {found:#?}"
        );
    }
}

#[test]
fn a_shell_read_of_the_scripts_variable_is_ours_and_another_scripts_read_is_not() {
    let world = snapshot_in(
        Lang::Shell,
        &[
            (
                "run.sh",
                "#!/bin/bash\nbin=\"${HOOK_BIN:-}\"\nif [[ -z \"$bin\" ]]; then\n  bin=x\nfi\n\"$bin\" run\n",
            ),
            ("other.sh", "#!/bin/bash\nbin=y\necho \"$bin\"\n"),
        ],
    );
    let subject = world.def("run.sh", "bin");
    let found = world.outcomes(subject, "run.sh", &["bin"]);
    for line in [3, 6] {
        assert_eq!(
            shape(on(&found, line)),
            ("ours", "resolved", Some(Relation::Reads)),
            "line {line}: {found:#?}"
        );
    }
    let found = world.outcomes(subject, "other.sh", &["bin"]);
    assert_eq!(
        shape(on(&found, 3)),
        ("not_ours", "other_definition", Some(Relation::Reads)),
        "{found:#?}"
    );
}

#[test]
fn a_shell_array_read_through_its_subscript_is_a_read_of_the_array() {
    let world = snapshot_in(
        Lang::Shell,
        &[(
            "run.sh",
            "#!/bin/bash\ngate=()\ngate=(--gate)\nrun \"${gate[@]}\"\nrun \"${#gate[@]}\"\n",
        )],
    );
    let found = world.outcomes(world.def("run.sh", "gate"), "run.sh", &["gate"]);
    for line in [4, 5] {
        assert_eq!(
            shape(on(&found, line)),
            ("ours", "resolved", Some(Relation::Reads)),
            "line {line}: {found:#?}"
        );
    }
}

/// The use of `subject` on `line` of `file` is ours, resolved, with `relation`.
fn assert_use(world: &World, subject: &Definition, file: &str, line: u32, relation: Relation) {
    let found = world.outcomes(subject, file, &[subject.key.local.name.as_str()]);
    assert_eq!(
        shape(on(&found, line)),
        ("ours", "resolved", Some(relation)),
        "{}:{line}: {found:#?}",
        subject.key.local.name.as_str()
    );
}

#[test]
fn a_python_chain_member_and_head_are_read_at_their_own_names() {
    let world = snapshot(&[
        (
            "cart.py",
            "class Items:\n    def total(self):\n        return 1\n\n\nclass Cart:\n    items: Items\n\n    def size(self):\n        return self.items.total()\n",
        ),
        (
            "app.py",
            "mcp = make()\n\n\n@mcp.tool()\ndef f():\n    return 1\n\n\nmcp.run()\n",
        ),
    ]);
    assert_use(
        &world,
        world.def("cart.py", "Cart.items"),
        "cart.py",
        10,
        Relation::Reads,
    );
    let mcp = world.def("app.py", "mcp");
    for line in [4, 9] {
        assert_use(&world, mcp, "app.py", line, Relation::Reads);
    }
}

#[test]
fn a_rust_chain_member_is_read_at_its_own_name_through_self_and_a_typed_local() {
    let world = snapshot_in(
        Lang::Rust,
        &[(
            "src/lib.rs",
            "pub struct Inner {\n    pub v: Vec<u8>,\n}\n\npub struct Outer {\n    pub a: Inner,\n}\n\nimpl Outer {\n    pub fn size(&self) -> usize {\n        self.a.v.len()\n    }\n}\n\npub fn of(c: &Outer) -> usize {\n    c.a.v.len()\n}\n",
        )],
    );
    for (owner, line) in [
        ("Outer.a", 11),
        ("Inner.v", 11),
        ("Outer.a", 16),
        ("Inner.v", 16),
    ] {
        assert_use(
            &world,
            world.def("src/lib.rs", owner),
            "src/lib.rs",
            line,
            Relation::Reads,
        );
    }
}

#[test]
fn a_rust_struct_literal_label_and_shorthand_write_the_field() {
    let world = snapshot_in(
        Lang::Rust,
        &[(
            "src/lib.rs",
            "pub struct S {\n    pub a: u8,\n}\n\npub fn make() -> S {\n    S { a: 1 }\n}\n\npub fn from(a: u8) -> S {\n    S { a }\n}\n",
        )],
    );
    for line in [6, 10] {
        assert_use(
            &world,
            world.def("src/lib.rs", "S.a"),
            "src/lib.rs",
            line,
            Relation::Writes,
        );
    }
}

/// A shorthand initializer writes the field and reads the binding it names: each reference is
/// judged against the subject on its own, so neither hides the other.
#[test]
fn a_shorthand_site_is_ours_through_whichever_reference_resolves_to_the_subject() {
    let rust = "pub fn total() -> i64 {\n    1\n}\n\npub struct Wrapper {\n    pub total: fn() -> i64,\n}\n\npub fn run() -> Wrapper {\n    let total =\n        Wrapper { total };\n    Wrapper { total }\n}\n";
    let world = snapshot_in(Lang::Rust, &[("src/lib.rs", rust)]);
    for line in [11, 12] {
        assert_use(
            &world,
            world.def("src/lib.rs", "Wrapper.total"),
            "src/lib.rs",
            line,
            Relation::Writes,
        );
    }
    let ts = "export interface Options {\n  total: number;\n}\n\nexport const total = 1;\n\nexport const options: Options = { total };\n";
    let world = snapshot_in(Lang::TypeScript, &[("src/options.ts", ts)]);
    assert_use(
        &world,
        world.def("src/options.ts", "Options.total"),
        "src/options.ts",
        7,
        Relation::Writes,
    );
    assert_use(
        &world,
        world.def("src/options.ts", "total"),
        "src/options.ts",
        7,
        Relation::Reads,
    );
}

#[test]
fn a_rust_type_written_before_a_path_is_read_at_its_own_name() {
    let world = snapshot_in(
        Lang::Rust,
        &[(
            "src/lib.rs",
            "pub struct S;\n\nimpl S {\n    pub fn zero() -> u8 {\n        0\n    }\n}\n\npub fn n() -> u8 {\n    S::zero()\n}\n",
        )],
    );
    assert_use(
        &world,
        world.def("src/lib.rs", "S"),
        "src/lib.rs",
        10,
        Relation::Reads,
    );
}

#[test]
fn a_go_chain_member_is_read_at_its_own_name() {
    let world = snapshot_in(
        Lang::Go,
        &[(
            "p.go",
            "package p\n\ntype Inner struct{ N int }\n\ntype Outer struct{ In Inner }\n\nfunc size(c Outer) int {\n\treturn c.In.N\n}\n",
        )],
    );
    assert_use(
        &world,
        world.def("p.go", "Outer.In"),
        "p.go",
        8,
        Relation::Reads,
    );
}

#[test]
fn a_trap_handler_names_the_function_it_runs() {
    let source = "#!/bin/bash\ncleanup() {\n  rm -f \"$1\"\n}\ntrap 'cleanup' EXIT\ntrap \"cleanup now\" INT\ntrap 'echo cleanup' HUP\n";
    let world = snapshot_in(Lang::Shell, &[("run.sh", source)]);
    let found = world.outcomes(world.def("run.sh", "cleanup"), "run.sh", &["cleanup"]);
    for line in [5, 6] {
        assert_eq!(
            shape(on(&found, line)),
            ("ours", "resolved", Some(Relation::Calls)),
            "line {line}: {found:#?}"
        );
    }
    assert_eq!(
        shape(on(&found, 7)),
        ("not_ours", "builtin", None),
        "{found:#?}"
    );
}

const IMPLS: &str = "pub struct QueryError;\n\nimpl Display for QueryError {\n    fn fmt(&self) {}\n}\n\nimpl<T> From<T> for QueryError {\n    fn from(_: T) -> Self {\n        QueryError\n    }\n}\n\nimpl QueryError {}\n\nfn wrap(e: impl QueryError) {}\n";

#[test]
fn the_type_an_impl_header_names_is_where_its_members_are_declared() {
    let world = snapshot_in(Lang::Rust, &[("src/lib.rs", IMPLS)]);
    let found = world.outcomes(
        world.def("src/lib.rs", "QueryError"),
        "src/lib.rs",
        &["QueryError"],
    );
    for line in [3, 7, 13] {
        assert_eq!(
            shape(on(&found, line)),
            ("declaration", "definition_name", None),
            "line {line}: {found:#?}"
        );
    }
}

#[test]
fn a_name_in_an_impl_trait_argument_is_no_impl_header_type() {
    let world = snapshot_in(Lang::Rust, &[("src/lib.rs", IMPLS)]);
    let found = world.outcomes(
        world.def("src/lib.rs", "QueryError"),
        "src/lib.rs",
        &["QueryError"],
    );
    assert_ne!(on(&found, 15).outcome, Outcome::Declaration, "{found:#?}");
    assert_ne!(on(&found, 9).outcome, Outcome::Declaration, "{found:#?}");
}

const TRAITS: &str = "pub trait Convert<T> {}\n\npub struct Page;\n\nimpl Convert<i32> for Page {\n}\n\nimpl<T> Convert<T> for Page {\n}\n\nimpl Page {}\n";

#[test]
fn the_trait_an_impl_header_names_is_no_declaration_of_it_but_the_type_is() {
    let world = snapshot_in(Lang::Rust, &[("src/lib.rs", TRAITS)]);
    let found = world.outcomes(
        world.def("src/lib.rs", "Convert"),
        "src/lib.rs",
        &["Convert"],
    );
    for line in [5, 8] {
        assert_ne!(
            on(&found, line).outcome,
            Outcome::Declaration,
            "line {line}: {found:#?}"
        );
    }
    let found = world.outcomes(world.def("src/lib.rs", "Page"), "src/lib.rs", &["Page"]);
    for line in [5, 8, 11] {
        assert_eq!(
            shape(on(&found, line)),
            ("declaration", "definition_name", None),
            "line {line}: {found:#?}"
        );
    }
}

/// (language, file, the subject's name, source, the line declaring the local, a plain read of it)
const LOCALS: [(Lang, &str, &str, &str, u32, u32); 5] = [
    (
        Lang::Rust,
        "src/lib.rs",
        "total",
        "pub fn total() -> i64 {\n    1\n}\n\npub fn run(n: i64) -> i64 {\n    let mut total = 0;\n    total += n;\n    total\n}\n",
        6,
        8,
    ),
    (
        Lang::Go,
        "shop/total.go",
        "Total",
        "package shop\n\nfunc Total() int {\n\treturn 1\n}\n\nfunc Run(n int) int {\n\tTotal := 0\n\tTotal += n\n\treturn Total\n}\n",
        8,
        10,
    ),
    (
        Lang::Php,
        "src/total.php",
        "total",
        "<?php\n\nfunction total() {\n    return 1;\n}\n\nfunction run($n) {\n    $total = 0;\n    $total += $n;\n    return $total;\n}\n",
        8,
        10,
    ),
    (
        Lang::C,
        "src/total.c",
        "total",
        "int total(void) {\n    return 1;\n}\n\nint run(const int *items, int n)\n{\n    int total = 0;\n    for (int i = 0; i < n; i++) {\n        total += items[i];\n    }\n    return total;\n}\n",
        7,
        11,
    ),
    (
        Lang::Shell,
        "run.sh",
        "total",
        "#!/bin/bash\ntotal() {\n  echo 1\n}\nrun() {\n  local total=0\n  echo \"$total\"\n}\n",
        6,
        7,
    ),
];

#[test]
fn a_local_is_declared_where_its_name_is_written_and_a_plain_read_of_it_is_not_ours() {
    for (lang, file, name, source, declared, read) in LOCALS {
        let world = snapshot_in(lang, &[(file, source)]);
        let found = world.outcomes(world.def(file, name), file, &[name]);
        assert_eq!(
            shape(on(&found, declared)),
            ("declaration", "local_site", None),
            "{file}: {found:#?}"
        );
        assert_eq!(
            shape(on(&found, read)),
            ("not_ours", "local", None),
            "{file}: {found:#?}"
        );
    }
}

/// (language, file, the subject's name, source, the line calling the subject)
const SHADOWS: [(Lang, &str, &str, &str, u32); 3] = [
    (
        Lang::Rust,
        "src/lib.rs",
        "path",
        "pub fn path() -> i64 {\n    1\n}\n\npub fn run() -> i64 {\n    let path =\n        path();\n    path\n}\n",
        7,
    ),
    (
        Lang::Php,
        "src/config.php",
        "config",
        "<?php\n\nfunction config() {\n    return 1;\n}\n\nfunction run() {\n    $config =\n        config();\n    return $config;\n}\n",
        9,
    ),
    (
        Lang::Shell,
        "run.sh",
        "helper",
        "#!/bin/bash\nhelper() {\n  echo 1\n}\nrun() {\n  local helper=1\n  helper\n  echo \"$helper\"\n}\n",
        7,
    ),
];

#[test]
fn a_call_of_the_name_a_local_also_bears_is_no_read_of_the_local() {
    for (lang, file, name, source, call) in SHADOWS {
        let world = snapshot_in(lang, &[(file, source)]);
        let found = world.outcomes(world.def(file, name), file, &[name]);
        assert_ne!(
            on(&found, call).outcome,
            Outcome::NotOurs,
            "{file}: {found:#?}"
        );
    }
}

/// (language, file, the subject's name, source, the line of an initializer that reads the outer
/// name as its own is written, the line of a read after the declaration)
const SELF_READS: [(Lang, &str, &str, &str, u32, u32); 3] = [
    (
        Lang::Rust,
        "src/lib.rs",
        "total",
        "pub fn total() -> i64 {\n    1\n}\n\npub struct Wrapper {\n    pub total: fn() -> i64,\n}\n\npub fn run() -> Wrapper {\n    let total =\n        Wrapper { total };\n    Wrapper { total }\n}\n",
        11,
        12,
    ),
    (
        Lang::Rust,
        "src/lib.rs",
        "total",
        "pub fn total() -> i64 {\n    1\n}\n\npub fn run() {\n    let total =\n        total;\n    let _ = total;\n}\n",
        7,
        8,
    ),
    (
        Lang::Go,
        "total.go",
        "total",
        "package shop\n\nfunc total() int {\n\treturn 1\n}\n\nfunc run() {\n\ttotal :=\n\t\ttotal\n\t_ = total\n}\n",
        9,
        10,
    ),
];

#[test]
fn an_initializer_reads_the_outer_name_and_what_follows_reads_the_local() {
    for (lang, file, name, source, initializer, later) in SELF_READS {
        let world = snapshot_in(lang, &[(file, source)]);
        let found = world.outcomes(world.def(file, name), file, &[name]);
        assert_eq!(
            on(&found, initializer).outcome,
            Outcome::Ours,
            "{file}:{initializer}: {found:#?}"
        );
        assert_eq!(
            shape(on(&found, later)),
            ("not_ours", "local", None),
            "{file}:{later}: {found:#?}"
        );
    }
}

#[test]
fn a_name_read_where_the_local_is_not_in_force_stays_a_candidate() {
    let source = "#!/bin/bash\ntotal() {\n  echo 1\n}\nrun() {\n  local total=0\n  echo \"$total\"\n}\nother() {\n  trap total EXIT\n}\n";
    let world = snapshot_in(Lang::Shell, &[("run.sh", source)]);
    let found = world.outcomes(world.def("run.sh", "total"), "run.sh", &["total"]);
    assert_eq!(
        on(&found, 10).outcome,
        Outcome::Candidate(Reason::Unexplained),
        "{found:#?}"
    );
}

#[test]
fn a_jvm_parameter_and_a_declarator_are_declared_at_the_name_and_a_call_in_the_initializer_is_not()
{
    let file = "src/main/java/shop/Pricing.java";
    let source = "package shop;\n\npublic class Pricing {\n    public static int total() {\n        return 1;\n    }\n\n    public static int add(int total) {\n        return total + 1;\n    }\n\n    public static int run() {\n        int total = total();\n        return total;\n    }\n}\n";
    let world = snapshot_in(Lang::Java, &[(file, source)]);
    let found = world.outcomes(world.def(file, "Pricing.total"), file, &["total"]);
    assert_eq!(
        shape(on(&found, 8)),
        ("declaration", "local_site", None),
        "{found:#?}"
    );
    let line: Vec<&Entry> = (found.entries.iter())
        .filter(|entry| entry.occurrence.line == 13)
        .collect();
    assert_eq!(line.len(), 2, "{found:#?}");
    assert_eq!(
        shape(line[0]),
        ("declaration", "local_site", None),
        "{found:#?}"
    );
    assert!(
        !matches!(line[1].outcome, Outcome::Declaration | Outcome::NotOurs),
        "{found:#?}"
    );
}

#[test]
fn a_call_bound_to_a_definition_and_its_prototype_is_ours() {
    let world = snapshot_in(
        Lang::C,
        &[
            ("src/total.h", "int total(void);\n"),
            (
                "src/total.c",
                "#include \"total.h\"\n\nint total(void) {\n    return 1;\n}\n",
            ),
            (
                "src/run.c",
                "#include \"total.h\"\n\nint run(void) {\n    return total();\n}\n",
            ),
        ],
    );
    for file in ["src/total.c", "src/total.h"] {
        let found = world.outcomes(world.def(file, "total"), "src/run.c", &["total"]);
        assert_eq!(
            shape(on(&found, 4)),
            ("ours", "resolved", Some(Relation::Calls)),
            "{file}: {found:#?}"
        );
    }
}

#[test]
fn a_segment_before_a_rust_path_separator_is_no_use_of_a_function_but_names_a_type() {
    let source = "pub fn config() -> i64 {\n    1\n}\n\npub struct Reply;\n\nimpl Reply {\n    pub fn new() -> Reply {\n        Reply\n    }\n}\n\npub fn run() -> i64 {\n    let _ = Reply::new();\n    config::load()\n}\n";
    let world = snapshot_in(Lang::Rust, &[("src/lib.rs", source)]);
    let found = world.outcomes(world.def("src/lib.rs", "config"), "src/lib.rs", &["config"]);
    assert_eq!(
        shape(on(&found, 15)),
        ("not_ours", "other_module", None),
        "{found:#?}"
    );
    let found = world.outcomes(world.def("src/lib.rs", "Reply"), "src/lib.rs", &["Reply"]);
    assert_ne!(on(&found, 14).outcome, Outcome::NotOurs, "{found:#?}");
}

#[test]
fn a_member_spelled_like_a_local_is_no_plain_read_of_it() {
    let source = "package shop\n\nfunc Total() int {\n\treturn 1\n}\n\nfunc Run(x any) int {\n\tTotal := 0\n\t_ = x.Total\n\treturn Total\n}\n";
    let world = snapshot_in(Lang::Go, &[("shop/total.go", source)]);
    let found = world.outcomes(
        world.def("shop/total.go", "Total"),
        "shop/total.go",
        &["Total"],
    );
    assert_ne!(on(&found, 9).outcome, Outcome::NotOurs, "{found:#?}");
}

#[test]
fn a_segment_of_a_package_name_is_no_use_of_a_definition_of_that_name() {
    for (lang, file, subject, source) in [
        (
            Lang::Java,
            "shop/total/Calc.java",
            "Calc.total",
            "package shop.total;\n\npublic class Calc {\n    public int total() {\n        return 1;\n    }\n}\n",
        ),
        (
            Lang::Kotlin,
            "shop/total/Calc.kt",
            "total",
            "package shop.total\n\nfun total(): Int {\n    return 1\n}\n",
        ),
    ] {
        let world = snapshot_in(lang, &[(file, source)]);
        let found = world.outcomes(world.def(file, subject), file, &["total"]);
        assert_eq!(
            shape(on(&found, 1)),
            ("not_ours", "other_module", None),
            "{file}: {found:#?}"
        );
    }
}
