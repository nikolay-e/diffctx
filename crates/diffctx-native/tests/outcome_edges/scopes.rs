//! Where a local is in force, read where the resolver states nothing: the text between a local's
//! site and its use decides a call, and the scope a callback owns decides which use a local of it
//! explains. Each expectation is read off the fixture's text.

use diffctx_facts::facts::{Fact, Local, LocalKind, Name, Scope};

use super::hand;
use crate::{
    Entry, FileOutcomes, LIB_F, Lang, Outcome, Reason, Resolution, UnresolvedReason, View, Witness,
    on, outcomes, snapshot, snapshot_in,
};

const UTIL: &str = "pub fn total(x: i64) -> i64 {\n    x\n}\n";

/// The spellings of `total` in `lib` with no binding stated at any of them.
fn unbound_in_lib(lib: &str) -> FileOutcomes {
    let mut world = snapshot_in(Lang::Rust, &[("src/lib.rs", lib), ("src/util.rs", UTIL)]);
    hand::unbind(&mut world, "src/lib.rs", "total");
    world.outcomes(world.def("src/util.rs", "total"), "src/lib.rs", &["total"])
}

fn is_local(entry: &Entry) -> bool {
    matches!(
        &entry.outcome,
        Outcome::NotOurs {
            witness: Witness::Local(_),
            ..
        }
    )
}

#[test]
fn an_unbound_call_of_a_closure_is_its_local_inside_the_block_and_no_local_after_it_closes() {
    let found = unbound_in_lib(
        "mod util;\n\npub fn run() -> i64 {\n    {\n        let total = |x: i64| x;\n        total(1);\n    }\n    total(2)\n}\n",
    );
    assert!(is_local(on(&found, 6)), "inside its block: {found:#?}");
    assert_eq!(
        on(&found, 8).outcome,
        Outcome::Candidate(Reason::Unexplained),
        "after its block closed: {found:#?}"
    );
}

#[test]
fn an_unbound_call_in_a_block_of_the_initializer_reads_the_outer_name_though_a_statement_ended_there()
 {
    let found = unbound_in_lib(
        "mod util;\n\npub fn run(flag: bool) -> i64 {\n    let total = if flag { prepare(); 1 } else { total(2) };\n    total\n}\n",
    );
    let call: Vec<&Entry> = found
        .entries
        .iter()
        .filter(|entry| entry.occurrence.line == 4)
        .collect();
    assert_eq!(call.len(), 2, "{found:#?}");
    assert_eq!(
        call[1].outcome,
        Outcome::Candidate(Reason::Unexplained),
        "the `;` inside the block ends no statement of the let: {found:#?}"
    );
}

#[test]
fn a_local_a_callback_owns_explains_a_use_in_that_callback_and_in_no_other_scope() {
    let mut world = snapshot(&[
        ("lib.py", LIB_F),
        (
            "app.py",
            "def main():\n    return f(1)\n\n\ndef other():\n    return f(2)\n",
        ),
    ]);
    let main = hand::scope_of(&world, "app.py", 2, "f");
    let callback = Scope::Callback {
        callee: vec![Name::new("then")],
        site: hand::span(&world, "app.py", "f(1)", 0),
        parent: Box::new(main),
    };
    let local = Local {
        name: Name::new("f"),
        site: hand::span(&world, "app.py", "main", 0),
        enclosing: callback.clone(),
        kind: LocalKind::Variable,
    };
    let local_use = Resolution::Unresolved {
        reason: UnresolvedReason::Local,
    };
    for line in [2, 6] {
        hand::rebind(&mut world, "app.py", line, "f", local_use.clone());
    }
    hand::written_from(&mut world, "app.py", 2, "f", callback);
    let read = hand::with_facts(&world, "app.py", vec![Fact::LocalBinding(local)]);
    let view = View::new(&world.files, &world.bindings, &world.definitions);
    let found = outcomes(
        &view,
        &read,
        world.text("app.py").as_bytes(),
        world.def("lib.py", "f"),
        &["f"],
    );
    assert!(is_local(on(&found, 2)), "{found:#?}");
    assert_eq!(
        on(&found, 6).outcome,
        Outcome::Candidate(Reason::BasisUnverified),
        "a callback's local reaches the callback and nothing around it: {found:#?}"
    );
}
