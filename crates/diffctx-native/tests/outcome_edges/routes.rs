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

fn read_at(files: &[(&str, &str)], line: u32) -> (&'static str, &'static str) {
    let world = snapshot(files);
    let found = world.outcomes(world.def("lib.py", "config"), "app.py", &["config"]);
    let here: Vec<_> = found
        .entries
        .iter()
        .filter(|entry| entry.occurrence.line == line)
        .collect();
    assert_eq!(here.len(), 1, "line {line}: {found:#?}");
    let shape = crate::shape(here[0]);
    (shape.0, shape.1)
}

const UNREACHED: (&str, &str) = ("not_ours", "unreached");
const OPEN: (&str, &str) = ("candidate", "unproven");

#[test]
fn a_name_the_subjects_file_defines_that_a_file_spells_without_a_binding_may_come_by_a_missed_import()
 {
    let lib = "def config():\n    return {}\n\n\ndef helper():\n    return 1\n";
    let spelled = "def g(load):\n    a = load()\n    helper()\n    return a.config\n";
    let other = "def g(load):\n    a = load()\n    elsewhere()\n    return a.config\n";
    assert_eq!(read_at(&[("lib.py", lib), ("app.py", spelled)], 4), OPEN);
    assert_eq!(read_at(&[("lib.py", lib), ("app.py", other)], 4), UNREACHED);
}

#[test]
fn a_file_the_script_subject_is_imported_by_may_be_handed_the_object_through_a_parameter() {
    let script = "def config():\n    return {}\n\n\nif __name__ == \"__main__\":\n    config()\n";
    let app = "def g(a):\n    return a.config\n";
    let imported = [
        ("lib.py", script),
        ("app.py", app),
        ("main.py", "import lib\n"),
    ];
    assert_eq!(read_at(&imported, 2), OPEN, "another file imports lib.py");
}

#[test]
fn a_parameter_of_a_function_the_file_does_not_call_stays_open_beside_other_names_it_uses() {
    let app = "def g(a):\n    return a.config\n\n\nvalue = 1 + other\n";
    assert_eq!(
        read_at(
            &[
                ("lib.py", "def config():\n    return {}\n"),
                ("app.py", app)
            ],
            2
        ),
        OPEN,
        "`other` is a reference of the top level, not a call of `g`"
    );
}

#[test]
fn a_file_that_loads_code_by_name_or_deserializes_may_bring_the_object_in() {
    let lib = "def config():\n    return {}\n";
    for (why, line) in [
        ("eval", "    eval(\"x\")\n"),
        ("exec", "    exec(\"x\")\n"),
        ("pickle.loads", "    pickle.loads(a)\n"),
    ] {
        let app = format!("def g(load):\n    a = load()\n{line}    return a.config\n");
        assert_eq!(
            read_at(&[("lib.py", lib), ("app.py", app.as_str())], 4),
            OPEN,
            "{why} runs code no import states"
        );
    }
    let app = "def g(load):\n    a = load()\n    handler = eval\n    return a.config\n";
    assert_eq!(
        read_at(&[("lib.py", lib), ("app.py", app)], 4),
        UNREACHED,
        "a mention of `eval` that calls nothing loads nothing"
    );
}

#[test]
fn two_reads_in_a_file_are_each_judged_by_the_name_their_own_chain_starts_at() {
    let app = "def first(svc):\n    return svc.config\n\n\ndef second(load):\n    b = load()\n    return b.config\n";
    let files = [
        ("lib.py", "def config():\n    return {}\n"),
        ("app.py", app),
    ];
    assert_eq!(
        read_at(&files, 2),
        OPEN,
        "a parameter of an uncalled function"
    );
    assert_eq!(read_at(&files, 7), UNREACHED, "a local the function builds");
}

#[test]
fn a_read_beside_an_earlier_read_of_a_parameter_in_one_function_is_judged_by_its_own_chain() {
    let app = "def g(svc, load):\n    first = svc.config\n    b = load()\n    return b.config\n";
    let files = [
        ("lib.py", "def config():\n    return {}\n"),
        ("app.py", app),
    ];
    assert_eq!(read_at(&files, 2), OPEN, "the parameter the function takes");
    assert_eq!(read_at(&files, 4), UNREACHED, "a local the function builds");
}

#[test]
fn a_chain_that_starts_at_a_name_nothing_in_the_file_binds_may_be_anything_at_run_time() {
    let lib = "def config():\n    return {}\n";
    let unbound = "def g():\n    return Svc.config\n";
    assert_eq!(read_at(&[("lib.py", lib), ("app.py", unbound)], 2), OPEN);
    let local = "def g(load):\n    svc = load()\n    return svc.config\n";
    assert_eq!(read_at(&[("lib.py", lib), ("app.py", local)], 3), UNREACHED);
    let defined = "svc = make()\n\n\ndef g():\n    return svc.config\n";
    assert_eq!(
        read_at(&[("lib.py", lib), ("app.py", defined)], 5),
        UNREACHED
    );
}

#[test]
fn a_read_inside_a_function_something_outside_enters_is_not_judged_unreached() {
    let app = "from flask import Flask\n\napp = Flask(__name__)\n\n\n@app.route(\"/\")\ndef index():\n    a = load()\n    return a.config\n";
    assert_eq!(
        read_at(
            &[
                ("lib.py", "def config():\n    return {}\n"),
                ("app.py", app)
            ],
            9
        ),
        OPEN
    );
}
