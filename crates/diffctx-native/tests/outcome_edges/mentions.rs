//! A name inside a string literal is a mention: counted (and counted as quoted when the literal
//! holds nothing else) unless a binding at the literal explains it, when it is code.

use std::sync::Arc;

use diffctx_occurrence::tokens::occurrence::Class;

use crate::{
    FileFacts, LIB_F, Lexical, Outcome, Relation, View, World, on, outcomes, shape, snapshot,
};

/// The facts of `file` with each of `literals` read as a string literal of the source.
fn with_strings(world: &World, file: &str, literals: &[&str]) -> FileFacts {
    let facts = world.facts(file);
    let text = world.text(file);
    let mut blob = (*facts.blob).clone();
    blob.lexical = literals
        .iter()
        .map(|literal| {
            let start = text.find(literal).expect("the literal") as u32;
            (start..start + literal.len() as u32, Lexical::String)
        })
        .collect();
    blob.in_source_order();
    FileFacts {
        blob: Arc::new(blob),
        ..facts.clone()
    }
}

#[test]
fn only_a_mention_alone_in_its_literal_is_counted_as_quoted() {
    let world = snapshot(&[
        ("lib.py", LIB_F),
        (
            "app.py",
            "def main():\n    a = \"f\"\n    b = \"say f twice\"\n    return a, b\n",
        ),
    ]);
    let read = with_strings(&world, "app.py", &["\"f\"", "\"say f twice\""]);
    let view = View::new(&world.files, &world.bindings, &world.definitions);
    let found = outcomes(
        &view,
        &read,
        world.text("app.py").as_bytes(),
        world.def("lib.py", "f"),
        &["f"],
    );
    assert!(found.entries.is_empty(), "{found:#?}");
    assert_eq!(found.mentions, 2, "{found:#?}");
    assert_eq!(found.quoted, 1, "{found:#?}");
}

#[test]
fn a_mention_a_binding_at_its_literal_explains_is_an_entry_read_as_code() {
    let mut world = snapshot(&[
        ("lib.py", LIB_F),
        (
            "app.py",
            "from lib import f\n\n\ndef main():\n    f()\n    return \"f\"\n",
        ),
    ]);
    let text = world.text("app.py");
    let start = text.find("\"f\"").expect("the literal") as u32;
    let mut claim = world
        .bindings
        .iter()
        .find(|b| b.file.as_str() == "app.py" && b.site.lines.0 == 5 && b.name.as_str() == "f")
        .expect("the call's binding")
        .clone();
    claim.site.bytes = start..start + 3;
    claim.site.lines = (6, 6);
    world.bindings.push(claim);
    let read = with_strings(&world, "app.py", &["\"f\""]);
    let view = View::new(&world.files, &world.bindings, &world.definitions);
    let found = outcomes(
        &view,
        &read,
        world.text("app.py").as_bytes(),
        world.def("lib.py", "f"),
        &["f"],
    );
    let entry = on(&found, 6);
    assert_eq!(entry.occurrence.class, Class::Code, "{found:#?}");
    assert_eq!(
        shape(entry),
        ("ours", "resolved", Some(Relation::Calls)),
        "{found:#?}"
    );
    assert_eq!(entry.outcome, Outcome::Ours, "{found:#?}");
    assert_eq!(found.mentions, 0, "{found:#?}");
}
