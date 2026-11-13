// The five classes of the occurrence accounting (arch migration, phase A).
// Each is a table of seeds under `support/occurrence/seeds/` and one test that
// runs it: the invariant of the class holds for every seed, or the answer
// that breaks it is printed with the seed's name. A class fails on a build
// whose "complete" does not come from explained occurrences.

mod support;
use support::families::Family;
#[path = "support/occurrence/mod.rs"]
mod occurrence;
use occurrence::{accounts_for, corpus, no_scaffold, run_seeds, scaffolds};

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
