//! A word of a Dockerfile instruction that holds shell text names a stage and no function of a
//! script: the row that reads it is for the stages' kind and the Dockerfile's family together.

use crate::{Lang, Outcome, Reason, snapshot_in};

#[test]
fn a_word_after_run_is_no_stage_name_for_a_function_of_a_script_and_stays_a_candidate() {
    let world = snapshot_in(
        Lang::Shell,
        &[
            ("scripts/build.sh", "#!/bin/bash\nbuild() {\n  echo hi\n}\n"),
            ("Dockerfile", "FROM scratch\nRUN build --all\n"),
        ],
    );
    let found = world.outcomes(
        world.def("scripts/build.sh", "build"),
        "Dockerfile",
        &["build"],
    );
    let word = found
        .entries
        .iter()
        .find(|entry| entry.occurrence.line == 2)
        .unwrap_or_else(|| panic!("no entry on line 2: {found:#?}"));
    assert_eq!(
        word.outcome,
        Outcome::Candidate(Reason::Unexplained),
        "{found:#?}"
    );
}
