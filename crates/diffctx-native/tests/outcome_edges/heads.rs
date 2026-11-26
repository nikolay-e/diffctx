//! A bare name heading a chain (`name.x`) and the object of a member read the resolver states at the
//! member. Each expectation is read off the fixture's text; the bindings a real resolver would
//! state at the head are dropped, so the rules that read an occurrence no binding names run.

use diffctx_facts::facts::Scope;

use super::hand;
use crate::{
    CONFIG, Evidence, EvidenceKind, Lang, Outcome, Reason, Relation, Resolution, Witness, World,
    on, shape, snapshot, snapshot_in,
};

fn module_of(file: &str) -> Resolution {
    Resolution::Module {
        file: crate::repo_path(file),
        evidence: Evidence {
            kind: EvidenceKind::ModuleAttribute,
            through: Vec::new(),
        },
    }
}

/// `source` as `app.py` beside the subject `config` of `lib.py` and a module `config.py`, with no
/// binding at the head of `config.get(...)`.
fn config_world(source: &str) -> World {
    let mut world = snapshot(&[
        ("lib.py", CONFIG),
        ("config.py", "VALUE = 1\n"),
        ("app.py", source),
    ]);
    hand::unbind(&mut world, "app.py", "config");
    world
}

/// The binding the import `statement` of `app.py` states for `config`.
fn import(world: &mut World, statement: &str, resolution: Resolution) {
    let site = hand::span(world, "app.py", statement, 0);
    let binding = hand::binding(
        "app.py",
        site,
        Scope::Module,
        Relation::Imports,
        "config",
        resolution,
    );
    world.bindings.push(binding);
}

fn head_of_chain(world: &World, line: u32) -> crate::Entry {
    let found = world.outcomes(world.def("lib.py", "config"), "app.py", &["config"]);
    on(&found, line).clone()
}

#[test]
fn a_bare_name_an_import_of_another_module_binds_is_that_module_and_not_the_subject() {
    let mut world = config_world("import config\n\n\ndef g():\n    return config.get(\"a\")\n");
    import(&mut world, "import config", module_of("config.py"));
    let head = head_of_chain(&world, 5);
    assert_eq!(
        shape(&head),
        ("not_ours", "other_module", None),
        "{head:#?}"
    );
}

#[test]
fn a_bare_name_the_file_defines_itself_is_no_proof_of_the_module_it_also_imports() {
    let mut world = config_world(
        "import config\n\n\ndef config():\n    return 1\n\n\ndef g():\n    return config.get(\"a\")\n",
    );
    import(&mut world, "import config", module_of("config.py"));
    let head = head_of_chain(&world, 9);
    assert_eq!(
        head.outcome,
        Outcome::Candidate(Reason::Unexplained),
        "{head:#?}"
    );
}

#[test]
fn a_bare_name_imported_twice_is_the_other_module_only_when_every_import_says_so() {
    let mut world = config_world(
        "try:\n    import config\nexcept ImportError:\n    from lib import config\n\n\ndef g():\n    return config.get(\"a\")\n",
    );
    let subject = hand::key_of(&world, "lib.py", "config");
    import(&mut world, "import config", module_of("config.py"));
    import(
        &mut world,
        "from lib import config",
        hand::resolved(&subject),
    );
    let head = head_of_chain(&world, 8);
    assert_eq!(
        head.outcome,
        Outcome::Candidate(Reason::Unexplained),
        "one of the imports may be the subject: {head:#?}"
    );
}

const PANEL: &str = "const PANEL = /x/;\n\nexport function go(t: string) {\n  return CALL;\n}\n";

/// `call` (a read of `PANEL` through `member`) in a function, with the binding of `member` the test
/// states and none at the object.
fn panel_read(call: &str, member: &str, lands_on: impl Fn(&World) -> Resolution) -> crate::Entry {
    let source = PANEL.replace("CALL", call);
    let mut world = snapshot_in(
        Lang::TypeScript,
        &[
            ("src/panel.ts", source.as_str()),
            (
                "src/other.ts",
                "export function exec(t: string) {\n  return t;\n}\n",
            ),
        ],
    );
    hand::unbind(&mut world, "src/panel.ts", "PANEL");
    hand::unbind(&mut world, "src/panel.ts", member);
    let site = hand::span(&world, "src/panel.ts", member, 0);
    let read = hand::binding(
        "src/panel.ts",
        site,
        hand::scope_of_def(&world, "src/panel.ts", "go"),
        Relation::Reads,
        member,
        lands_on(&world),
    );
    world.bindings.push(read);
    let found = world.outcomes(
        world.def("src/panel.ts", "PANEL"),
        "src/panel.ts",
        &["PANEL"],
    );
    on(&found, 4).clone()
}

#[test]
fn the_object_of_a_member_read_is_the_read_the_member_states_however_the_member_is_spelled() {
    for (call, member) in [
        ("PANEL.exec(t)", "exec"),
        ("PANEL .exec(t)", "exec"),
        ("PANEL. exec(t)", "exec"),
        ("PANEL\n    .exec(t)", "exec"),
        ("PANEL.exec_all(t)", "exec_all"),
        ("PANEL.$exec(t)", "$exec"),
    ] {
        let object = panel_read(call, member, |world| {
            hand::resolved(&hand::key_of(world, "src/panel.ts", "PANEL"))
        });
        assert_eq!(
            shape(&object),
            ("ours", "resolved", Some(Relation::Reads)),
            "{call:?}: {object:#?}"
        );
    }
}

#[test]
fn the_object_of_a_member_read_that_lands_on_another_definition_is_no_read_of_the_subject() {
    let object = panel_read("PANEL.exec(t)", "exec", |world| {
        hand::resolved(&hand::key_of(world, "src/other.ts", "exec"))
    });
    assert_eq!(
        object.outcome,
        Outcome::Candidate(Reason::Unexplained),
        "{object:#?}"
    );
}

const SHAPE: &str = "export class Shape {\n  scale(): number {\n    return 1;\n  }\n}\n\nexport namespace Shape {\n  export const unit = 1;\n}\n\nexport function use(): string {\n  return scale.toFixed(1) + unit.toFixed(2);\n}\n";

fn bare_in_shape(subject: &str, name: &str) -> crate::Entry {
    let mut world = snapshot_in(Lang::TypeScript, &[("src/shape.ts", SHAPE)]);
    for dropped in ["scale", "unit", "toFixed"] {
        hand::unbind(&mut world, "src/shape.ts", dropped);
    }
    let found = world.outcomes(world.def("src/shape.ts", subject), "src/shape.ts", &[name]);
    on(&found, 12).clone()
}

#[test]
fn a_member_of_a_class_merged_with_a_namespace_is_no_bare_name_outside_the_class() {
    let member = bare_in_shape("Shape.scale", "scale");
    assert!(
        matches!(
            member.outcome,
            Outcome::NotOurs {
                witness: Witness::BareName(_),
                ..
            }
        ),
        "a member of the class needs a receiver outside its body: {member:#?}"
    );
}

#[test]
fn a_member_the_merged_namespace_declares_is_no_member_of_the_class_and_keeps_its_hedge() {
    let member = bare_in_shape("Shape.unit", "unit");
    assert_eq!(
        member.outcome,
        Outcome::Candidate(Reason::Unexplained),
        "{member:#?}"
    );
}
