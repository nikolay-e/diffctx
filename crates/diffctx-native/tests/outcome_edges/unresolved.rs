//! What each reason a resolver gave for an unresolved use proves, read at a use whose resolution
//! the test sets to that reason: a witness is claimed only where the text supplies it.

use std::sync::Arc;

use crate::{
    Entry, FileFacts, LIB_F, Lang, Outcome, Reason, Relation, Resolution, UnresolvedReason, View,
    Witness, World, build, outcomes, shape, snapshot, snapshot_in,
};

fn rebind(world: &mut World, file: &str, line: u32, name: &str, reason: UnresolvedReason) {
    let binding = world
        .bindings
        .iter_mut()
        .find(|b| b.file.as_str() == file && b.site.lines.0 == line && b.name.as_str() == name)
        .unwrap_or_else(|| panic!("no binding of {name} at {file}:{line}"));
    binding.resolution = Resolution::Unresolved { reason };
}

fn only(entries: &[Entry], line: u32) -> &Entry {
    let here: Vec<&Entry> = entries
        .iter()
        .filter(|entry| entry.occurrence.line == line)
        .collect();
    assert_eq!(here.len(), 1, "line {line}: {entries:#?}");
    here[0]
}

#[test]
fn a_local_no_local_of_the_name_in_force_explains_stays_unverified() {
    let mut world = snapshot(&[
        ("lib.py", LIB_F),
        (
            "app.py",
            "def other():\n    f = 1\n    return f\n\n\ndef main():\n    x = 1\n    return f()\n",
        ),
    ]);
    rebind(&mut world, "app.py", 8, "f", UnresolvedReason::Local);
    let found = world.outcomes(world.def("lib.py", "f"), "app.py", &["f"]);
    assert_eq!(
        only(&found.entries, 8).outcome,
        Outcome::Candidate(Reason::BasisUnverified),
        "{found:#?}"
    );
}

#[test]
fn an_external_name_the_repository_has_a_module_for_proves_nothing_external() {
    let mut world = snapshot(&[
        ("lib.py", LIB_F),
        (
            "app.py",
            "from lib import f\n\n\ndef main():\n    return f()\n",
        ),
    ]);
    rebind(&mut world, "app.py", 5, "f", UnresolvedReason::External);
    let found = world.outcomes(world.def("lib.py", "f"), "app.py", &["f"]);
    assert_eq!(
        only(&found.entries, 5).outcome,
        Outcome::Candidate(Reason::BasisUnverified),
        "{found:#?}"
    );
}

#[test]
fn an_external_name_no_import_binds_is_no_definition_a_non_global_language_removed() {
    let mut world = build(
        &[
            ("lib.py", LIB_F),
            ("app.py", "def main():\n    return f()\n"),
        ],
        &[("lib.py", "def other():\n    return 1\n")],
    );
    rebind(&mut world, "app.py", 2, "f", UnresolvedReason::External);
    let removed = crate::find(&world.base_definitions, "lib.py", "f");
    let found = world.outcomes(removed, "app.py", &["f"]);
    assert!(
        matches!(
            &only(&found.entries, 2).outcome,
            Outcome::NotOurs {
                witness: Witness::ExternalName(_),
                ..
            }
        ),
        "{found:#?}"
    );
}

#[test]
fn an_external_name_no_import_binds_is_no_function_a_shell_script_defines() {
    let source = "#!/bin/bash\ncleanup() {\n  rm -f \"$1\"\n}\ncleanup now\n";
    let mut world = snapshot_in(Lang::Shell, &[("run.sh", source)]);
    rebind(
        &mut world,
        "run.sh",
        5,
        "cleanup",
        UnresolvedReason::External,
    );
    let found = world.outcomes(world.def("run.sh", "cleanup"), "run.sh", &["cleanup"]);
    assert!(
        matches!(
            &only(&found.entries, 5).outcome,
            Outcome::NotOurs {
                witness: Witness::ExternalName(_),
                ..
            }
        ),
        "{found:#?}"
    );
}

#[test]
fn two_bindings_that_agree_on_not_being_ours_are_not_ours() {
    let mut world = snapshot(&[
        ("lib.py", LIB_F),
        ("other.py", "def f():\n    return 2\n"),
        (
            "app.py",
            "from other import f\n\n\ndef main():\n    return f()\n",
        ),
    ]);
    let call = world
        .bindings
        .iter()
        .find(|b| b.file.as_str() == "app.py" && b.site.lines.0 == 5 && b.name.as_str() == "f")
        .expect("the call's binding")
        .clone();
    world.bindings.push(call);
    let found = world.outcomes(world.def("lib.py", "f"), "app.py", &["f"]);
    assert_eq!(
        shape(only(&found.entries, 5)),
        ("not_ours", "other_definition", Some(Relation::Calls)),
        "{found:#?}"
    );
}

#[test]
fn a_module_read_with_a_loss_proves_no_use_of_it_is_not_ours() {
    let world = snapshot(&[
        ("util.py", "X = 1\n"),
        ("other.py", "def util():\n    return 2\n"),
        (
            "app.py",
            "import util\n\n\ndef use():\n    return repr(util)\n",
        ),
    ]);
    let facts = world.facts("util.py");
    let mut blob = (*facts.blob).clone();
    blob.parse_error(vec![(1, 1)]);
    let lossy = FileFacts {
        blob: Arc::new(blob),
        ..facts.clone()
    };
    let files: Vec<FileFacts> = (world.files.iter())
        .map(|file| {
            if file.path.as_str() == "util.py" {
                lossy.clone()
            } else {
                file.clone()
            }
        })
        .collect();
    let view = View::new(&files, &world.bindings, &world.definitions);
    let found = outcomes(
        &view,
        world.facts("app.py"),
        world.text("app.py").as_bytes(),
        world.def("other.py", "util"),
        &["util"],
    );
    assert_eq!(
        only(&found.entries, 5).outcome,
        Outcome::Candidate(Reason::BasisUnverified),
        "{found:#?}"
    );
}
