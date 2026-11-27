//! A routine a schema or a header states twice is one object: a binding to the other statement of
//! the subject's own key is the subject's own, in the family that pairs them and in no other. The
//! bindings are written by hand where no resolver binds across languages.

use diffctx_facts::facts::{DefKey, Language, Scope};

use super::hand;
use crate::{Lang, Outcome, Reason, Relation, World, on, shape, snapshot_in};

const FIRST: &str = "CREATE SCHEMA lib;\n\nCREATE FUNCTION lib.refresh_book(target_album UUID) RETURNS INTEGER\nLANGUAGE plpgsql AS $$\nBEGIN\n  RETURN 1;\nEND;\n$$;\n";
const REDONE: &str = "CREATE OR REPLACE FUNCTION lib.refresh_book(target_album UUID) RETURNS INTEGER\nLANGUAGE plpgsql AS $$\nBEGIN\n  RETURN 0;\nEND;\n$$;\n";
const CALLER: &str = "CREATE OR REPLACE FUNCTION lib.refresh_all() RETURNS void\nLANGUAGE plpgsql AS $$\nBEGIN\n  PERFORM lib.refresh_book(1);\nEND;\n$$;\n";

/// The call on line 4 of the caller, bound as `targets` say and by no other binding.
fn sql_call(targets: impl Fn(&World) -> Vec<DefKey>) -> crate::Entry {
    let mut world = snapshot_in(
        Lang::Sql,
        &[
            ("db/V1__fn.sql", FIRST),
            ("db/V2__use.sql", CALLER),
            ("db/V3__redo.sql", REDONE),
        ],
    );
    hand::unbind(&mut world, "db/V2__use.sql", "refresh_book");
    let site = hand::span(&world, "db/V2__use.sql", "refresh_book", 0);
    for target in targets(&world) {
        let call = hand::binding(
            "db/V2__use.sql",
            site.clone(),
            Scope::Module,
            Relation::Calls,
            "refresh_book",
            hand::resolved(&target),
        );
        world.bindings.push(call);
    }
    let subject = world.def("db/V1__fn.sql", "lib.refresh_book");
    let found = world.outcomes(subject, "db/V2__use.sql", &["refresh_book"]);
    on(&found, 4).clone()
}

#[test]
fn a_call_bound_to_the_routine_and_to_its_redefinition_is_ours() {
    let call = sql_call(|world| {
        vec![
            hand::key_of(world, "db/V1__fn.sql", "lib.refresh_book"),
            hand::key_of(world, "db/V3__redo.sql", "lib.refresh_book"),
        ]
    });
    assert_eq!(call.outcome, Outcome::Ours, "{call:#?}");
}

#[test]
fn a_call_bound_to_a_definition_of_another_language_is_no_redefinition_of_the_routine() {
    let call = sql_call(|world| {
        let subject = hand::key_of(world, "db/V1__fn.sql", "lib.refresh_book");
        vec![DefKey {
            language: Language::Python,
            ..subject
        }]
    });
    assert_eq!(
        call.outcome,
        Outcome::Candidate(Reason::BasisUnverified),
        "{call:#?}"
    );
}

#[test]
fn a_call_bound_to_a_definition_of_another_language_is_no_pair_of_a_prototype() {
    let mut world = snapshot_in(
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
    let prototype = hand::key_of(&world, "src/total.h", "total");
    hand::unbind(&mut world, "src/run.c", "total");
    let call = hand::binding(
        "src/run.c",
        hand::within(&world, "src/run.c", "return total();", "total"),
        hand::scope_of_def(&world, "src/run.c", "run"),
        Relation::Calls,
        "total",
        hand::resolved(&DefKey {
            language: Language::Python,
            ..prototype
        }),
    );
    world.bindings.push(call);
    let found = world.outcomes(world.def("src/total.h", "total"), "src/run.c", &["total"]);
    assert_eq!(
        shape(on(&found, 4)),
        ("candidate", "unproven", Some(Relation::Calls)),
        "{found:#?}"
    );
}
