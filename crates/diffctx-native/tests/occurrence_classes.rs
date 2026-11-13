// The five classes of the occurrence accounting (arch migration, phase A).
// Each is a table of seeds under `support/occurrence/seeds/` and one test that
// runs it: the invariant of the class holds for every seed, or the answer
// that breaks it is printed with the seed's name. A class fails on a build
// whose "complete" does not come from explained occurrences.

use std::collections::BTreeSet;

use _diffctx::facts::EntryKind;

mod support;
use support::families::{Family, discloses, says_empty};
#[path = "support/occurrence/mod.rs"]
mod occurrence;
use occurrence::{
    ABSENCE, ALIASES, IMPLICIT, LEAKS, accounts_for, calls_complete, corpus, covers,
    engine_implicit_kinds, load, no_scaffold, run_seeds, says_nobody, scaffolds,
};

#[test]
fn every_family_has_a_scaffold_or_a_reason() {
    for family in Family::ALL {
        let has = scaffolds().contains_key(family.label());
        assert_eq!(
            has,
            no_scaffold(family).is_none(),
            "{}: the scaffold table and `no_scaffold` disagree",
            family.label()
        );
        assert!(
            has || !family.proves_empty(),
            "{}: states a clean absence and has no scaffold",
            family.label()
        );
    }
}

// Class 1. Whatever the position, a file that spells the definition's name in
// code is named by the answer or the answer says what it did not read: a
// position the engine cannot explain is a candidate, never silence.
#[test]
fn an_occurrence_is_not_lost() {
    let mut seeds = Vec::new();
    for family in Family::ALL {
        match corpus::seeds(family) {
            Ok(found) => {
                assert!(!found.is_empty(), "{}: no corpus position", family.label());
                seeds.extend(found);
            }
            Err(reason) => eprintln!("{}: not read: {reason}", family.label()),
        }
    }
    run_seeds("an occurrence is not lost", &seeds, |seed, doc| {
        if accounts_for(doc, &seed.user) {
            Ok(())
        } else {
            Err(format!(
                "the answer neither names {} nor discloses",
                seed.user
            ))
        }
    });
}

// Class 2. The definition is used under another name (a renaming import, a
// re-export, an assignment, a star import, up to the closure's bound): the
// answer names the user or says what it could not follow, and is never a
// clean absence. A chain longer than the bound is a named gap, not a stop.
#[test]
fn an_alias_of_the_definition_is_not_lost() {
    let seeds = load(ALIASES);
    covers(
        "aliases",
        &seeds,
        &[("shell", "a shell function is called by its name only")],
    );
    run_seeds("an alias is not lost", &seeds, |seed, doc| {
        if accounts_for(doc, &seed.user) && !says_empty(doc) {
            Ok(())
        } else {
            Err(format!("the answer loses the user {}", seed.user))
        }
    });
}

// Class 3. A definition handed on as a value (an argument, a return value, a
// collection member, a field) can be called by a place the index cannot see,
// so its `calls` relation is partial however many direct calls are listed.
// The `direct` seed of each family is the control: a plain call leaves the
// relation complete.
#[test]
fn a_value_leak_makes_the_calls_relation_partial() {
    let seeds = load(LEAKS);
    covers(
        "leaks",
        &seeds,
        &[("shell", "a shell function is called by its name only")],
    );
    run_seeds("a value leak", &seeds, |seed, doc| {
        let expected = seed.kind == "direct";
        match calls_complete(doc, seed.symbol()) {
            Some(complete) if complete == expected => Ok(()),
            found => Err(format!(
                "a {} use leaves the calls relation complete = {found:?}, expected {expected}",
                seed.kind
            )),
        }
    });
}

// The kinds of the contract's table of implicit calls. A kind of the engine's
// `Implicit` that has no row here, or a row the engine lacks, fails the suite.
const IMPLICIT_KINDS: [&str; 6] = [
    "operator", "protocol", "override", "entry", "test", "outside",
];

// The seed of each way the engine knows a framework or a language enters a
// definition: a full match, so a new `EntryKind` stops this file compiling
// until it has a seed.
fn entry_seed(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Main => "rust_main",
        EntryKind::ScriptGuard => "python_script_guard",
        EntryKind::Test => "python_test_function",
        EntryKind::Command => "python_click_command",
        EntryKind::Route => "python_flask_route",
        EntryKind::Scheduled => "java_spring_scheduled",
        EntryKind::Tool => "python_mcp_tool",
        EntryKind::Fixture => "python_pytest_fixture",
    }
}

// Class 4. A definition invoked without its name written (an operator, a
// protocol method, an override of an outside base, an entry, a test, an
// export) is never reported as called by nobody: the answer names it with the
// kind of the unknown or lists the entry that reaches it.
#[test]
fn an_implicit_call_is_never_nobody() {
    let seeds = load(IMPLICIT);
    let kinds: BTreeSet<&str> = seeds.iter().map(|s| s.kind.as_str()).collect();
    assert_eq!(
        kinds,
        BTreeSet::from(IMPLICIT_KINDS),
        "a kind has no seed, or a seed has no kind"
    );
    if let Some(engine) = engine_implicit_kinds() {
        assert_eq!(
            engine,
            IMPLICIT_KINDS
                .iter()
                .map(|k| (*k).to_string())
                .collect::<BTreeSet<String>>(),
            "the engine's `Implicit` and the seeds' kinds differ"
        );
    }
    for kind in [
        EntryKind::Main,
        EntryKind::ScriptGuard,
        EntryKind::Test,
        EntryKind::Command,
        EntryKind::Route,
        EntryKind::Scheduled,
        EntryKind::Tool,
        EntryKind::Fixture,
    ] {
        let name = entry_seed(kind);
        assert!(
            seeds.iter().any(|s| s.name == name),
            "{kind:?}: no seed named {name}"
        );
    }
    run_seeds("an implicit call", &seeds, |seed, doc| {
        if says_nobody(doc, seed.symbol()) {
            Err(format!("the answer says nothing calls {}", seed.symbol()))
        } else {
            Ok(())
        }
    });
}

// Class 5. The name is spelled in a file of the group, and every spelling is
// one the contract explains (a comment, a string, a local that shadows it):
// the answer is still a clean absence. The class guards the switch's
// decisiveness: an engine that hedges at every spelling passes the other four.
#[test]
fn a_clean_absence_stays_clean() {
    let seeds = load(ABSENCE);
    covers("absence", &seeds, &[]);
    run_seeds("an absence", &seeds, |seed, doc| {
        if says_empty(doc) && !discloses(doc) {
            Ok(())
        } else {
            Err(format!(
                "a {} of the name is not a clean absence",
                seed.kind
            ))
        }
    });
}
