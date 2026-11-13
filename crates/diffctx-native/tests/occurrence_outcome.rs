//! The outcome of each occurrence (`occurrence::outcomes`) on real Python files
//! committed to a real repository and read by the real resolver: each row of the
//! contract's two tables that today's facts can produce, the `f(g(f))` trap, a
//! `not_ours` whose basis the accounting cannot verify, and the base side. The
//! expectations are read off the sources below, never off a run of diffctx.

mod snapshot_fixture;

use std::sync::Arc;

use _diffctx::facts::{DefKind, FactsCache, FileFacts, Lexical, facts_of};
use _diffctx::resolve::{
    Binding, CandidateReason, Definition, Evidence, EvidenceKind, PythonResolver, Relation,
    Resolution, TypeScriptResolver, UnresolvedReason,
};
use _diffctx::snapshot::{AnalysisContext, RangeSpec, RepoPath, RunBudget, Side, SideTag};
use _diffctx::tokens::occurrence::{
    Basis, Entry, FileOutcomes, Outcome, Reason, View, admitted, counts_against, outcomes,
};
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
        ("declaration", "local_site", None),
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
fn a_not_ours_the_accounting_cannot_verify_stays_a_candidate() {
    let world = snapshot(&[
        ("lib.py", "def join(a):\n    return a\n"),
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
        "the resolver says External, but no import binds `join` itself: the head of the chain is not carried"
    );
    assert_eq!(call.relation, Some(Relation::Calls));
    assert_ne!(call.outcome.as_str(), "not_ours");
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
        ("app.py", "def h(a):\n    return a.f.g()\n"),
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
        Outcome::Candidate(Reason::Unexplained),
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
        ("not_ours", "other_definition", None),
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
        ("app.py", "def h(a):\n    return a.f.g()\n"),
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
