//! The public API a changed definition moves.

use diffctx_base::types::Fragment;

use super::super::changes::{
    is_public, rel, signature_changed, tool_contract_changed, without_decorators,
};
use super::super::output::{Caller, Contract};

use super::context::{Findings, Impact};
use super::definition::Core;

/// Lists the definition as a moved `public_api` contract when its place in the surface changed.
pub(super) fn note_public_api(
    cx: &Impact<'_>,
    c: &Core<'_, '_>,
    callers: &[Caller],
    found: &mut Findings,
) {
    let state = cx.state;
    let query = cx.query;
    let tests = &cx.tests;
    let base_texts = &cx.base_texts;
    let is_tool = |f: &Fragment| cx.is_tool(f);
    let core = c.core;
    let py = c.py;
    let symbol = c.symbol;
    // An export's place in the surface moves when it is added, renamed or
    // moved, or its kind or signature changes: the facts layer says it for
    // its languages (Kotlin's cell also says which are public), the
    // declaration text for the rest.
    let public_change = match py
        .filter(|c| diffctx_base::languages::get_language_for_file(&c.file) == Some("kotlin"))
    {
        Some(changed) => changed.api,
        // A tool states no visibility: it is published to every agent that
        // lists it, whatever the language spells.
        None if is_tool(core) => tool_contract_changed(state, base_texts, core),
        None => {
            is_public(core.path(), symbol, without_decorators(&core.content))
                && super::super::rust::module_is_public(&state.run.source(), core.path())
                && py.map_or_else(|| signature_changed(state, base_texts, core), |def| def.api)
        }
    };
    // A test's declarations are no API: nothing but its runner calls
    // them (#517).
    if query.is_none() && public_change && !tests.is_test(core) {
        found.public_api.push((
            callers.is_empty(),
            Contract {
                path: rel(state, core.path()),
                symbol: core.symbol_name.clone(),
                kind: "public_api",
            },
        ));
    }
}
