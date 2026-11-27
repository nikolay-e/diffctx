//! The definitions with no fragment of their own: each traced from the facts layer's entry alone.

use crate::answer::render::place;

use crate::decide::{self};

use diffctx_base::types::Fragment;

use super::super::cells::{cell_caller, cell_symbol, held_has_users, lifted_caller};
use super::super::changes::lines_of;
use super::super::coverage::absence;
use super::super::deliver::relation_rank;
use super::super::dispatch::merge_by_symbol;
use super::super::output::Caller;

use super::context::{Findings, Impact};
use super::targets::Targets;

/// Traces every entry the facts layer reports that no fragment anchors; returns the ones the deadline
/// kept it from reaching.
pub(super) fn trace<'t>(
    cx: &Impact<'_>,
    t: &'t Targets<'_>,
    found: &mut Findings,
) -> Vec<&'t decide::changed::Changed> {
    let (state, query, cells, tests, files) = (cx.state, cx.query, cx.cells, &cx.tests, &cx.files);
    let (cell_apart, untraced) = (&t.cell_apart, &t.untraced);
    let test_links = |frag, core: Option<&Fragment>| cx.test_links(frag, core);
    // The definition has no fragment of its own (a one-line class holds its
    // method on its own lines), but each use sits in one: the caller's row is
    // that fragment's, with the test links it has. A fragment that names no
    // definition (a Dockerfile or a Terraform module cut as one chunk) holds the
    // line but not the caller: the resolver's row names the stage or the block.
    let in_fragment = |row: Caller| -> Caller {
        let Some(frag) = row.sites.first().and_then(|&site| {
            let frags = files
                .fragments
                .get(diffctx_base::paths::join_native(&state.root_dir, &row.path).as_str())?;
            cell_caller(frags, site).map(|f| lifted_caller(frags, f))
        }) else {
            return row;
        };
        let found = test_links(frag, None);
        let (symbol, lines) = match &frag.symbol_name {
            Some(name) => (Some(name.clone()), lines_of(&frag.id)),
            None => (row.symbol.clone(), row.lines.clone()),
        };
        Caller {
            symbol,
            lines,
            tests: found.tests,
            registered_by: found.registered_by,
            file_tested: found.file_tested,
            unread_test: found.unread_test,
            tests_open: found.tests_open,
            tests_partial: found.tests_partial,
            ..row
        }
    };
    let mut traced_apart = 0u64;
    let mut apart_unreached: Vec<&decide::changed::Changed> = Vec::new();
    for entry in cell_apart {
        let Some(cell) = cells.of(&entry.file) else {
            continue;
        };
        // The facts' hash can miss what the syntax changed (the default of a promoted
        // constructor property): the claim stands unless a hunk of its lines is not layout.
        let contradicted = state.hunk_texts.iter().any(|h| {
            let (a, b) = h.new_range();
            h.path == entry.file && !h.layout && entry.lines.0 <= b && entry.lines.1 >= a
        });
        if query.is_none()
            && !contradicted
            && matches!(
                entry.how,
                decide::changed::How::Edited { layout_only: true }
            )
        {
            found.layout_only.push(place(
                &entry.file,
                Some(entry.name.as_str()).filter(|_| entry.symbol().is_some()),
                &format!("{}-{}", entry.lines.0, entry.lines.1),
            ));
            continue;
        }
        // Past the deadline a definition is named as never searched, as the
        // loop over the fragments does for its own.
        if !state.run.check() {
            apart_unreached.push(entry);
            continue;
        }
        traced_apart += 1;
        let verdict = cell.verdict(entry);
        let mut sym = cell_symbol(entry, &verdict, format!("{:?}", entry.kind).to_lowercase());
        let rows = std::mem::take(&mut sym.callers);
        sym.callers = merge_by_symbol(rows.into_iter().map(in_fragment).collect());
        sym.callers.sort_by_key(|c| relation_rank(c.relation));
        untraced.name_literals(&mut sym, entry, &verdict);
        untraced.name_text(&mut sym, &entry.name, verdict.names_quoted());
        let callerless = query.is_none()
            && sym.callers.is_empty()
            && sym.also_changed.is_empty()
            && sym.possibly_also_changed.is_empty();
        if callerless && verdict.complete() && held_has_users(cell, entry) {
            continue;
        }
        found.traced_definitions += 1;
        found.traced_names.push(entry.name.clone());
        // A definition with no fragment has no test link computed: with tests in
        // view it keeps its row, which claims none; the list that says "no static
        // test link" is for the answer that has no tests to say it against.
        let named = place(&sym.path, sym.symbol.as_deref(), &sym.lines);
        let in_a_test_file = tests.is_test_file(&sym.path);
        let all = sym.coverage.all();
        match all.witness().filter(|_| callerless && verdict.complete()) {
            Some(_) if in_a_test_file => {
                found
                    .entry_unlisted
                    .push(format!("{named} \u{2014} test (run by the test runner)"));
            }
            Some(complete) if !tests.any() => {
                found.untested_unlisted.push(absence(named, complete))
            }
            _ => found.changed.push(sym),
        }
    }
    diffctx_base::profile::count("impact.traced_apart", traced_apart);
    apart_unreached
}
