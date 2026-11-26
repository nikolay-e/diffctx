//! Whether an object of the subject's class can come to a file by an import, read at an untyped
//! member read: the files below import each other or not, and the expectation follows from that.

use crate::{CONFIG, Outcome, Reason, UnresolvedReason, snapshot};

#[test]
fn a_file_some_file_imports_may_be_handed_an_object_no_import_of_its_own_leads_to() {
    let app = "def g(load):\n    a = load()\n    return a.config\n";
    let world = snapshot(&[
        ("lib.py", CONFIG),
        ("app.py", app),
        ("main.py", "import app\n"),
    ]);
    let found = world.outcomes(world.def("lib.py", "config"), "app.py", &["config"]);
    let here: Vec<_> = found
        .entries
        .iter()
        .filter(|entry| entry.occurrence.line == 3)
        .collect();
    assert_eq!(here.len(), 1, "{found:#?}");
    assert_eq!(
        here[0].outcome,
        Outcome::Candidate(Reason::Unresolved(UnresolvedReason::UntypedRead)),
        "main.py imports app.py and may hand it any object: {found:#?}"
    );
}
