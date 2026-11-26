//! How the bindings at an occurrence settle into one claim: a candidate a class's members make, an
//! import that may bind a name, a declaration a binding also names, a use bound wider than its name,
//! a renamed import. A resolution a real resolver does not state for the fixture is written by hand
//! over a binding it did state.

use diffctx_facts::facts::{Fact, Scope};

use super::hand;
use crate::{
    CandidateReason, LIB_F, Lang, OVER, Outcome, Reason, Relation, Resolution, UnresolvedReason,
    World, declared_at, on, shape, snapshot, snapshot_in,
};

fn candidates_of(world: &mut World, file: &str, line: u32, name: &str, reason: CandidateReason) {
    let binding = world
        .bindings
        .iter_mut()
        .find(|b| b.file.as_str() == file && b.site.lines.0 == line && b.name.as_str() == name)
        .unwrap_or_else(|| panic!("no binding of {name} at {file}:{line}"));
    match &mut binding.resolution {
        Resolution::Candidate { reason: held, .. } => *held = reason,
        other => panic!("a candidate resolution was expected: {other:?}"),
    }
}

#[test]
fn a_java_candidate_no_class_row_names_keeps_its_reason_whatever_the_overloads_left_out() {
    let mut world = snapshot_in(Lang::Java, &[("o/Over.java", OVER)]);
    candidates_of(
        &mut world,
        "o/Over.java",
        17,
        "add",
        CandidateReason::UntypedReceiver,
    );
    let subject = declared_at(&world.definitions, "o/Over.java", 12);
    let found = world.outcomes(subject, "o/Over.java", &["add"]);
    assert_eq!(
        on(&found, 17).outcome,
        Outcome::Candidate(Reason::Resolver(CandidateReason::UntypedReceiver)),
        "{found:#?}"
    );
}

#[test]
fn a_candidate_of_a_class_row_reason_in_another_language_keeps_its_reason() {
    let mut world = snapshot(&[(
        "shapes.py",
        "class Sq:\n    def a(self):\n        return 1\n\n    def b(self):\n        return 2\n\n\ndef use(s):\n    return s.a()\n",
    )]);
    let sibling = hand::key_of(&world, "shapes.py", "Sq.b");
    hand::rebind(
        &mut world,
        "shapes.py",
        10,
        "a",
        Resolution::Candidate {
            targets: vec![sibling],
            reason: CandidateReason::Overloaded,
        },
    );
    let found = world.outcomes(world.def("shapes.py", "Sq.a"), "shapes.py", &["a"]);
    assert_eq!(
        on(&found, 10).outcome,
        Outcome::Candidate(Reason::Resolver(CandidateReason::Overloaded)),
        "only a Java class reads its overloads off one class's members: {found:#?}"
    );
}

const JOIN: [(&str, &str); 2] = [
    ("lib.py", "def join(a):\n    return a\n"),
    (
        "app.py",
        "from os.path import join\n\n\ndef here():\n    return join(\"a\")\n",
    ),
];

#[test]
fn an_import_that_names_the_name_is_the_module_it_comes_from_and_no_builtin() {
    let mut world = snapshot(&JOIN);
    let external = Resolution::Unresolved {
        reason: UnresolvedReason::External,
    };
    hand::rebind(&mut world, "app.py", 5, "join", external);
    let found = world.outcomes(world.def("lib.py", "join"), "app.py", &["join"]);
    assert_eq!(
        shape(on(&found, 5)),
        ("not_ours", "external_module", Some(Relation::Calls)),
        "{found:#?}"
    );
    let builtin = Resolution::Unresolved {
        reason: UnresolvedReason::Builtin,
    };
    hand::rebind(&mut world, "app.py", 5, "join", builtin);
    let found = world.outcomes(world.def("lib.py", "join"), "app.py", &["join"]);
    assert_eq!(
        on(&found, 5).outcome,
        Outcome::Candidate(Reason::BasisUnverified),
        "the import names it, so it is no language builtin: {found:#?}"
    );
}

#[test]
fn an_override_name_beside_a_family_member_is_that_declaration_and_no_use_of_the_subject() {
    let world = snapshot(&[(
        "kinds.py",
        "class A:\n    def run(self):\n        return 1\n\n\nclass B(A):\n    def run(self):\n        return 2\n\n\nclass C(A):\n    def run(self):\n        return 3\n",
    )]);
    let found = world.outcomes(world.def("kinds.py", "B.run"), "kinds.py", &["run"]);
    assert_eq!(
        shape(on(&found, 12)),
        ("declaration", "definition_name", None),
        "`C.run` overrides `A.run`, which `B.run` also does: it is a declaration of another definition: {found:#?}"
    );
}

#[test]
fn a_use_bound_wider_than_its_name_to_two_definitions_is_a_conflict() {
    let mut world = snapshot(&[
        ("lib.py", LIB_F),
        ("other.py", "def f():\n    return 2\n"),
        ("app.py", "def main():\n    return f(1)\n"),
    ]);
    let from = hand::scope_of(&world, "app.py", 2, "f");
    hand::unbind(&mut world, "app.py", "f");
    let site = hand::span(&world, "app.py", "f(1)", 0);
    for (file, dotted) in [("lib.py", "f"), ("other.py", "f")] {
        let target = hand::resolved(&hand::key_of(&world, file, dotted));
        let wide = hand::binding(
            "app.py",
            site.clone(),
            from.clone(),
            Relation::Calls,
            "f",
            target,
        );
        world.bindings.push(wide);
    }
    let found = world.outcomes(world.def("lib.py", "f"), "app.py", &["f"]);
    assert_eq!(
        on(&found, 2).outcome,
        Outcome::Candidate(Reason::Conflict),
        "one use, bound to the subject and to another definition: {found:#?}"
    );
}

#[test]
fn a_use_bound_over_an_import_that_renames_is_that_use_and_no_alias() {
    let mut world = snapshot(&[("lib.py", LIB_F), ("app.py", "from lib import f as g\n")]);
    let import = world
        .facts("app.py")
        .blob
        .facts
        .iter()
        .find_map(|fact| match fact {
            Fact::Import(import) => Some(import.clone()),
            _ => None,
        })
        .expect("the import");
    let subject = hand::key_of(&world, "lib.py", "f");
    let over = hand::binding(
        "app.py",
        import.site,
        Scope::Module,
        Relation::Calls,
        "f",
        hand::resolved(&subject),
    );
    world.bindings.insert(0, over);
    let found = world.outcomes(world.def("lib.py", "f"), "app.py", &["f"]);
    assert_eq!(on(&found, 1).outcome, Outcome::Ours, "{found:#?}");
}
