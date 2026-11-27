//! A binding to a module is the subject's own only when the module offers the subject on: a
//! re-export of it, never a module that merely uses it.

use crate::{Lang, Relation, shape, snapshot, snapshot_in};

#[test]
fn a_module_that_only_calls_the_subject_does_not_offer_it_on() {
    let world = snapshot(&[
        (
            "util.py",
            "import other\n\n\ndef helper():\n    return other.util()\n",
        ),
        ("other.py", "def util():\n    return 2\n"),
        (
            "app.py",
            "import util\n\n\ndef use():\n    return repr(util)\n",
        ),
    ]);
    let found = world.outcomes(world.def("other.py", "util"), "app.py", &["util"]);
    let here: Vec<_> = found
        .entries
        .iter()
        .filter(|entry| entry.occurrence.line == 5)
        .collect();
    assert_eq!(here.len(), 1, "{found:#?}");
    assert_eq!(
        shape(here[0]),
        ("not_ours", "other_module", Some(Relation::PassesAsValue)),
        "{found:#?}"
    );
}

fn namespace_use(index: &str) -> (&'static str, &'static str, Option<Relation>) {
    namespace_use_where(index, true)
}

/// `kept` says whether the resolver's re-export bindings of `src/index.ts` stay.
fn namespace_use_where(index: &str, kept: bool) -> (&'static str, &'static str, Option<Relation>) {
    let mut world = snapshot_in(
        Lang::TypeScript,
        &[
            (
                "src/m.ts",
                "export function thing(): number {\n  return 1;\n}\n",
            ),
            (
                "src/other.ts",
                "export function other(): number {\n  return 2;\n}\n",
            ),
            ("src/index.ts", index),
            (
                "src/app.ts",
                "import * as idx from \"./index\";\n\nexport const used = [idx];\n",
            ),
        ],
    );
    if !kept {
        world.bindings.retain(|binding| {
            !(binding.file.as_str() == "src/index.ts" && binding.relation == Relation::Reexports)
        });
    }
    let found = world.outcomes(world.def("src/m.ts", "thing"), "src/app.ts", &["idx"]);
    let here: Vec<_> = found
        .entries
        .iter()
        .filter(|entry| entry.occurrence.line == 3)
        .collect();
    assert_eq!(here.len(), 1, "{found:#?}");
    shape(here[0])
}

#[test]
fn a_module_offers_the_subject_on_by_naming_it_or_by_a_star_and_not_by_naming_another() {
    let named = namespace_use("export { thing } from \"./m\";\n");
    let star = namespace_use("export * from \"./m\";\n");
    let other = namespace_use("export { other } from \"./other\";\n");
    assert_eq!(named.0, "ours", "{named:?}");
    assert_eq!(star.0, "ours", "{star:?}");
    assert_eq!(other.0, "not_ours", "{other:?}");
}

#[test]
fn a_module_naming_the_subject_in_an_export_offers_it_on_without_a_binding_to_say_so() {
    let named = namespace_use_where("export { thing } from \"./m\";\n", false);
    let other = namespace_use_where("export { other } from \"./other\";\n", false);
    assert_eq!(named.0, "ours", "{named:?}");
    assert_eq!(other.0, "not_ours", "{other:?}");
}
