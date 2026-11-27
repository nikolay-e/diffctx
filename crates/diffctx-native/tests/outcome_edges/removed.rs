//! What a head use is when the subject is a definition the range removed: a same-named definition
//! it lands on is a candidate while the subject's family is unread, and what the base said of the
//! use is kept at the place it was, the surer of two.

use diffctx_occurrence::tokens::occurrence::Standing;

use super::hand;
use crate::{
    Binding, CandidateReason, LIB_F, Outcome, Reason, Resolution, View, World, build, find, on,
    outcomes,
};

fn removed_world() -> World {
    build(
        &[
            ("lib.py", LIB_F),
            (
                "app.py",
                "from lib import f\n\n\ndef main():\n    return f()\n",
            ),
        ],
        &[
            ("lib.py", "def other():\n    return 1\n"),
            (
                "app2.py",
                "def f():\n    return 2\n\n\ndef run():\n    return f()\n",
            ),
        ],
    )
}

#[test]
fn a_same_named_definition_a_removed_ones_use_lands_on_is_a_candidate_while_its_family_is_unread() {
    let world = removed_world();
    let removed = find(&world.base_definitions, "lib.py", "f");
    let view = View::new(&world.files, &world.bindings, &world.definitions).removed_sites(
        None,
        std::slice::from_ref(&removed.key),
        |_| Standing::Resolved,
        |_| false,
    );
    let found = outcomes(
        &view,
        world.facts("app2.py"),
        world.text("app2.py").as_bytes(),
        removed,
        &["f"],
    );
    assert_eq!(
        on(&found, 6).outcome,
        Outcome::Candidate(Reason::OverrideUnproven),
        "{found:#?}"
    );
}

#[test]
fn a_place_the_base_resolved_to_a_removed_definition_keeps_the_surer_standing_whatever_the_order() {
    let world = removed_world();
    let key = find(&world.base_definitions, "lib.py", "f").key.clone();
    let call = world
        .bindings
        .iter()
        .find(|b| b.file.as_str() == "app2.py" && b.name.as_str() == "f" && b.site.lines.0 == 6)
        .expect("the head call");
    let held = Resolution::Candidate {
        targets: vec![key.clone()],
        reason: CandidateReason::UntypedReceiver,
    };
    let sure = hand::resolved(&key);
    for order in [[held.clone(), sure.clone()], [sure, held]] {
        let base: Vec<Binding> = order
            .into_iter()
            .map(|resolution| Binding {
                resolution,
                ..call.clone()
            })
            .collect();
        let view = View::new(&world.files, &world.bindings, &world.definitions).removed_sites(
            Some(&base),
            std::slice::from_ref(&key),
            |binding| match binding.resolution {
                Resolution::Resolved { .. } => Standing::Resolved,
                _ => Standing::Candidate("untyped_receiver"),
            },
            |_| false,
        );
        let standing: Vec<Standing> = view
            .stranded()
            .filter(|(binding, _)| binding.name.as_str() == "f" && binding.site.lines.0 == 6)
            .map(|(_, standing)| standing)
            .collect();
        assert_eq!(standing, [Standing::Resolved]);
    }
}
