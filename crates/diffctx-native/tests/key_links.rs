//! The stop-rule gate for typed key links: of the links the key table draws
//! to a file the change did not touch, how many land on a file the case asks
//! for.
//!
//! Over the cases in the directories `DIFFCTX_KEY_LINK_CASES` names
//! (comma-separated, relative to `tests/cases/diff`; default
//! `infrastructure/keys` and `infrastructure/xlang`, whichever exist), every
//! typed link with exactly one changed endpoint is scored by what sits at the
//! other end: a file holding a required fragment is right, a forbidden one is
//! wrong, anything else is unlabelled and counted apart. A kind with at least
//! `MIN_LINKS` counted links must reach `MIN_PRECISION`, or the test fails.
//! Recall is reported per kind over the cases tagged for it (`xkeys:<kind>`,
//! `xlang:<kind>`). Run with `-- --nocapture` for the table and the wrong
//! links.

use std::collections::{BTreeMap, BTreeSet};

use _diffctx::in_memory_harness::typed_key_links;

#[allow(dead_code)]
mod common;
mod key_cases;
use key_cases::{Label, Tally, far_end};

const MIN_PRECISION: f64 = 0.85;
/// Below this many counted links a kind is reported, not gated.
const MIN_LINKS: usize = 5;
const KINDS: [&str; 5] = ["env", "image", "route", "table", "proto"];

#[test]
fn typed_key_links_are_precise() {
    let dirs = key_cases::case_dirs(
        "DIFFCTX_KEY_LINK_CASES",
        &["infrastructure/keys", "infrastructure/xlang"],
    );
    let cases = key_cases::load_cases(&dirs);
    assert!(!cases.is_empty(), "no cases under {dirs:?}");

    let mut by_kind: BTreeMap<&'static str, Tally> = BTreeMap::new();
    let mut required: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut reached: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut wrong: Vec<String> = Vec::new();

    for case in &cases {
        let mut links: BTreeMap<
            (&'static str, String, String),
            (String, &'static str, &'static str),
        > = BTreeMap::new();
        for l in typed_key_links(&case.repo) {
            let (a, b) = if l.a <= l.b { (l.a, l.b) } else { (l.b, l.a) };
            links
                .entry((l.kind, a, b))
                .or_insert((l.key, l.a_mechanism, l.b_mechanism));
        }

        let mut hit: BTreeMap<&'static str, BTreeSet<String>> = BTreeMap::new();
        for ((kind, a, b), (key, mech_a, mech_b)) in &links {
            let Some(far) = far_end(case, a, b) else {
                continue;
            };
            let label = case.label(far);
            by_kind.entry(kind).or_default().add(label);
            match label {
                Label::Right => {
                    hit.entry(kind).or_default().insert(far.to_string());
                }
                Label::Wrong => wrong.push(format!(
                    "{}: {kind} `{key}` {a} <-> {b} ({mech_a} / {mech_b})",
                    case.name
                )),
                Label::Unlabelled => {}
            }
        }

        let wanted = case.required_files();
        for kind in KINDS {
            let tagged = case
                .test
                .tags
                .iter()
                .any(|t| t == &format!("xkeys:{kind}") || t == &format!("xlang:{kind}"));
            if !tagged {
                continue;
            }
            *required.entry(kind).or_default() += wanted.len();
            *reached.entry(kind).or_default() +=
                hit.get(kind).map_or(0, |h| h.intersection(&wanted).count());
        }
    }

    println!("\ncases: {}", cases.len());
    for kind in KINDS {
        let tally = by_kind.get(kind).copied().unwrap_or_default();
        println!(
            "  {kind:<6} {}  recall {}/{}",
            tally.describe(),
            reached.get(kind).copied().unwrap_or(0),
            required.get(kind).copied().unwrap_or(0)
        );
    }
    println!(
        "  with n of 20-40 links, 0.85 tolerates 3-6 wrong ones: the gate holds, its power is low"
    );
    for line in &wrong {
        println!("  wrong: {line}");
    }

    let failing: Vec<String> = by_kind
        .iter()
        .filter(|(_, t)| {
            t.counted() >= MIN_LINKS && t.precision().is_some_and(|p| p < MIN_PRECISION)
        })
        .map(|(kind, t)| format!("{kind}: {}", t.describe()))
        .collect();
    assert!(
        failing.is_empty(),
        "typed key links below {MIN_PRECISION} precision:\n{}",
        failing.join("\n")
    );
}
