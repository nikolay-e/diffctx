//! A keyword argument's name is the field or parameter of the call that holds it, never of a call
//! written inside the arguments before it.

use crate::{Relation, shape, snapshot};

#[test]
fn a_keyword_after_a_nested_call_writes_the_field_of_the_class_the_outer_call_builds() {
    let world = snapshot(&[
        (
            "lib.py",
            "class Reply:\n    url: str = \"\"\n\n\ndef inner(x):\n    return x\n",
        ),
        (
            "app.py",
            "from lib import Reply, inner\n\n\ndef make():\n    return Reply(inner(1), url=\"x\")\n",
        ),
    ]);
    let found = world.outcomes(world.def("lib.py", "Reply.url"), "app.py", &["url"]);
    let here: Vec<_> = found
        .entries
        .iter()
        .filter(|entry| entry.occurrence.line == 5)
        .collect();
    assert_eq!(here.len(), 1, "{found:#?}");
    assert_eq!(
        shape(here[0]),
        ("ours", "resolved", Some(Relation::Writes)),
        "{found:#?}"
    );
}

#[test]
fn a_keyword_of_another_class_with_a_field_of_the_same_name_is_no_write_of_this_one() {
    let world = snapshot(&[
        (
            "lib.py",
            "class Reply:\n    url: str = \"\"\n\n\nclass Other:\n    url: str = \"\"\n",
        ),
        (
            "app.py",
            "from lib import Other\n\n\ndef make():\n    return Other(url=\"y\")\n",
        ),
    ]);
    let found = world.outcomes(world.def("lib.py", "Reply.url"), "app.py", &["url"]);
    for entry in found.entries.iter().filter(|e| e.occurrence.line == 5) {
        assert_ne!(
            shape(entry),
            ("ours", "resolved", Some(Relation::Writes)),
            "{found:#?}"
        );
    }
}

#[test]
fn a_keyword_of_a_class_of_the_same_name_in_another_module_is_no_write_of_this_one() {
    let world = snapshot(&[
        ("lib.py", "class Reply:\n    url: str = \"\"\n"),
        ("elsewhere.py", "class Reply:\n    url: str = \"\"\n"),
        (
            "app.py",
            "from elsewhere import Reply\n\n\ndef make():\n    return Reply(url=\"y\")\n",
        ),
    ]);
    let found = world.outcomes(world.def("lib.py", "Reply.url"), "app.py", &["url"]);
    for entry in found.entries.iter().filter(|e| e.occurrence.line == 5) {
        assert_ne!(
            shape(entry),
            ("ours", "resolved", Some(Relation::Writes)),
            "{found:#?}"
        );
    }
}
