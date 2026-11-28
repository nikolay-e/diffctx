//! What implicitly calls a changed definition and what the trace could not resolve of it: its entry
//! role, the route a framework dispatches it by, and the unknown it leaves.

use crate::decide::{self};

use diffctx_base::types::Fragment;

use diffctx_facts::facts::framework;

use super::super::callers::is_code_file;
use super::super::cells::cell_unknown;
use super::super::dispatch::{base_symbol, dispatch_route, enclosing_class};
use super::super::entry::{Entered, http_handler_base, is_program_main};
use super::super::output::{Caller, Unknown};

use super::context::Impact;
use super::definition::Core;

pub(super) struct Role {
    pub(super) route: Option<String>,
    pub(super) entry: Option<String>,
    pub(super) linkless_role: bool,
    pub(super) unresolved: Option<Unknown>,
}

pub(super) fn of(
    cx: &Impact<'_>,
    c: &Core<'_, '_>,
    callers: &[Caller],
    also_changed: &[String],
    entered: &Option<Entered>,
) -> Role {
    let state = cx.state;
    let tests = &cx.tests;
    let by_id = &cx.by_id;
    let definitions = &cx.definitions;
    let file_of = |f: &Fragment| cx.file_of(f);
    let registration = |f: &Fragment| cx.registration(f);
    let link = |user: &Fragment, used: &Fragment| cx.link(user, used);
    let core = c.core;
    let py = c.py;
    let verdict = &c.verdict;
    let symbol = c.symbol;
    let core_lang = c.core_lang;
    let ambiguous = c.ambiguous;
    // A resolved caller inside the diff is a caller: the framework route a
    // definition nothing calls may take is not claimed over it. A possible one
    // proves nothing, and the route stands.
    let resolved_in_diff = !also_changed.is_empty();
    // A changed symbol nothing depends on is the diff itself; the reader
    // holds that already, and listing it spends the cap on nothing.
    // A queried symbol's definition is half the answer: where it is.
    // Unless nothing could be resolved: an empty list is then no finding,
    // and silence would read as one. A trait method is reached through
    // its trait, whatever else shares its name (`fmt` in `impl Display`).
    // A data field or a class is no call target a framework dispatches to.
    let dispatchable = py.is_none_or(|def| {
        matches!(
            def.kind,
            diffctx_facts::facts::DefKind::Method | diffctx_facts::facts::DefKind::Property
        )
    });
    let route = (callers.is_empty() && !resolved_in_diff && dispatchable)
        .then(|| {
            dispatch_route(state, core, file_of(core)).or_else(|| {
                verdict
                    .as_ref()
                    .and_then(decide::verdict::Verdict::overridden_via)
                    .map(str::to_string)
            })
        })
        .flatten();
    // The role that gives the definition an implicit caller: where a
    // static test link exists it is shown, otherwise the role is named.
    let test_link = |frag| {
        let found = tests.tested_by(frag, state, by_id, &link);
        (!found.is_empty()).then(|| super::super::text::reachable_from_tests(&found))
    };
    let mut linkless_role = false;
    // A test helper the range's own tests call is traced like any definition:
    // the runner enters only what no one calls.
    let entry: Option<String> = if tests.is_test(core) && also_changed.is_empty() {
        Some("test (run by the test runner)".to_string())
    } else if matches!(entered, Some(Entered::Script)) {
        Some("run as a script (if __name__ == \"__main__\")".to_string())
    } else if let Some(by) = registration(core).or_else(|| match &entered {
        Some(Entered::Registered(call)) => Some(call.clone()),
        _ => None,
    }) {
        Some(
            match (by.starts_with(framework::ENTRY_PREFIX), test_link(core)) {
                (true, Some(link)) => format!("{by}; {link}"),
                (true, None) => by,
                (false, Some(link)) => format!("registered by `{by}`; {link}"),
                (false, None) => format!(
                    "registered by `{by}` \u{2014} reached through the framework, not traced"
                ),
            },
        )
    } else if let Some(base) = http_handler_base(core, file_of(core)) {
        // The class link is modelled: no test reaching it is a finding,
        // not an unmodelled gap, so the role does not excuse it.
        let link = enclosing_class(core, file_of(core)).and_then(test_link);
        Some(match link {
            Some(link) => format!("dispatched by `{base}`; {link}"),
            None => {
                linkless_role = true;
                format!("dispatched by `{base}`")
            }
        })
    } else if is_program_main(core) {
        Some(match test_link(core) {
            Some(link) => format!("program entry point; {link}"),
            // A test that runs the binary is modelled for Rust: none
            // reaching `main` is a finding there, not an unmodelled gap.
            None if core.path().ends_with(".rs") => {
                linkless_role = true;
                "program entry point".to_string()
            }
            None => "program entry point".to_string(),
        })
    } else {
        None
    };
    let unresolved = symbol
        .filter(|_| ambiguous)
        .map(|s| Unknown::Ambiguous {
            name: s.to_string(),
            definitions: definitions
                .get(&(core_lang, s.to_lowercase()))
                .copied()
                .unwrap_or(0),
            language: core_lang,
        })
        .or_else(|| route.clone().map(|via| Unknown::Overridable { via }))
        .or_else(|| {
            // The rows a text search lists are no resolution: the gap stands
            // beside them, and an empty list is never read as no caller.
            (py.is_none() && c.class_use.is_none() && is_code_file(core.path())).then(|| {
                Unknown::NotModelled {
                    language: if core_lang.is_empty() {
                        "this language"
                    } else {
                        core_lang
                    },
                }
            })
        });
    // An entry point is called by its runner or framework: the hedge for
    // callers nothing traces would be a false unknown.
    let unresolved = unresolved.filter(|u| {
        entry.is_none()
            || !matches!(
                u,
                Unknown::Overridable { .. }
                    | Unknown::NotModelled { .. }
                    | Unknown::Dispatched { .. }
            )
    });
    // What the resolver could not land is said, never read as no caller.
    let shown = py.map_or_else(
        || symbol.map(base_symbol).unwrap_or("").to_string(),
        |def| def.name.clone(),
    );
    // A route no model traces is named by the answer's own reason when it has one.
    let routed = matches!(unresolved, Some(Unknown::Overridable { .. }));
    let unresolved = verdict
        .as_ref()
        .and_then(decide::verdict::Verdict::gap)
        .filter(|gap| {
            !(routed
                && matches!(
                    gap,
                    decide::gap::Gap::NotTraced
                        | decide::gap::Gap::PassedAsValue { .. }
                        | decide::gap::Gap::ImplicitlyConstructed
                ))
        })
        .map(|gap| cell_unknown(&gap, &shown))
        .or(unresolved);
    // Read after the search: a stop noted while it ran, or before it
    // began, leaves its callers a lower bound.
    let stops = state.run.stops();
    let unresolved = if stops.is_empty() {
        unresolved
    } else {
        Some(Unknown::Unfinished { stops })
    };
    Role {
        route,
        entry,
        linkless_role,
        unresolved,
    }
}
