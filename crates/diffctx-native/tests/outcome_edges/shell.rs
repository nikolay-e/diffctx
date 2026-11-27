use crate::{Lang, Outcome, Witness, snapshot_in};

#[test]
fn a_word_a_longer_word_holds_names_that_whole_word() {
    let source = "#!/bin/bash\ncleanup() {\n  rm -f \"$1\"\n}\nbash cleanup.sh\nmake cleanup-all\ntar --cleanup x\n";
    let world = snapshot_in(Lang::Shell, &[("run.sh", source)]);
    let found = world.outcomes(world.def("run.sh", "cleanup"), "run.sh", &["cleanup"]);
    for (line, word) in [(5, "cleanup.sh"), (6, "cleanup-all"), (7, "--cleanup")] {
        let entry = found
            .entries
            .iter()
            .find(|entry| entry.occurrence.line == line)
            .expect("an entry");
        let Outcome::NotOurs {
            witness: Witness::Compound(name),
            ..
        } = &entry.outcome
        else {
            panic!("line {line}: {found:#?}");
        };
        assert_eq!(name.as_str(), word);
    }
}
