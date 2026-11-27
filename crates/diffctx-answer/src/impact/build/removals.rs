//! Definitions the range removed: who still reads each of them at the head.

use std::path::Path;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::decide::{self};

use diffctx_base::types::{Fragment, FragmentId};

use diffctx_facts::facts::EntryKind;

use super::super::cells::cell_symbol;
use super::super::changes::{head_of, rel};
use super::super::dispatch::base_symbol;
use super::super::output::{ChangedSymbol, Contract, Unknown};
use super::super::removed::{removed_definitions, removed_route};

use super::context::{Findings, Impact};
use super::targets::Targets;

pub(super) fn read(cx: &Impact<'_>, t: &Targets<'_>, found: &mut Findings) {
    let (state, query, cells, tests, by_id, analysed) = (
        cx.state,
        cx.query,
        cx.cells,
        &cx.tests,
        &cx.by_id,
        cx.analysed,
    );
    let (cell_removed, untraced, defined_in) = (&t.cell_removed, &t.untraced, &cx.defined_in);
    let in_a_hunk = |id: &FragmentId| cx.in_a_hunk(id);
    let link = |user: &Fragment, used: &Fragment| cx.link(user, used);
    let reaches_file = |test: &Fragment, caller: &Fragment| cx.reaches_file(test, caller);
    let removed = diffctx_base::profile::phase("impact.removed");
    if query.is_none() && state.run.check() {
        let mut defined: FxHashMap<&str, FxHashSet<String>> = FxHashMap::default();
        for (n, p) in defined_in {
            defined.entry(n.as_str()).or_default().insert(rel(state, p));
        }
        let head_rev = head_of(analysed);
        // The facts layer answers for the files of the cells' languages
        // (`cell_removed`).
        let read_off_facts = |path: &str| cells.of(path).is_some();
        let mut listed_alone: FxHashSet<(String, String)> = FxHashSet::default();
        let mut from_facts: Vec<(ChangedSymbol, Option<Contract>)> = Vec::new();
        // A removed name is listed with its readers, or with what could not be
        // told, or alone (#699). A removed public definition is a contract that
        // moved, listed or not.
        from_facts.extend(cell_removed.iter().filter_map(|entry| {
            let verdict = cells.of(&entry.file)?.verdict(entry);
            let mut sym = cell_symbol(entry, &verdict, "removed".to_string());
            untraced.name_removed(&mut sym, entry, &verdict);
            // A removed override was reached through the member it overrode: the
            // answer names that route, whatever else it could not tell.
            if let Some(via) = verdict.overridden_via() {
                if matches!(sym.unresolved, None | Some(Unknown::Uncovered { .. })) {
                    sym.unresolved = Some(Unknown::Overridable {
                        via: via.to_string(),
                    });
                }
            }
            // A removed route handler is reached over HTTP by clients no model
            // traces: a test that requests the route is one of them.
            if sym.unresolved.is_none() {
                let cell = cells.of(&entry.file)?;
                let name = base_symbol(&entry.name);
                let route = cell
                    .removed_entries_at(&entry.file, name, entry.lines)
                    .into_iter()
                    .find(|(kind, _)| *kind == EntryKind::Route);
                if let Some((_, route)) = route {
                    sym.unresolved = Some(removed_route(
                        state,
                        analysed,
                        &|path| tests.is_test_file(path),
                        &route,
                    ));
                    sym.coverage =
                        decide::coverage::Coverages::partial(decide::gap::Gap::NotTraced);
                }
            }
            // Every removed definition of a cell language is reported, private ones
            // too: with the readers the head still has, or as one nothing references.
            listed_alone.insert((sym.path.clone(), entry.display_name()));
            let contract = entry.public.then(|| Contract {
                path: sym.path.clone(),
                symbol: entry.symbol(),
                kind: "removed_api",
            });
            Some((sym, contract))
        }));
        for (sym, contract) in removed_definitions(
            state,
            &defined,
            head_rev.as_deref(),
            &in_a_hunk,
            &read_off_facts,
        )
        .into_iter()
        .chain(from_facts)
        {
            let mut sym = sym;
            // A caller of a removed definition is tested the way any caller
            // is: a test that reaches it, or the test file it sits in (#413).
            for c in &mut sym.callers {
                let Some(site) = c.sites.first().copied() else {
                    continue;
                };
                let Some(frag) = state
                    .all_fragments
                    .iter()
                    .filter(|f| {
                        f.symbol_name == c.symbol
                            && f.start_line() <= site
                            && f.end_line() >= site
                            && rel(state, f.path()) == c.path
                    })
                    .min_by_key(|f| f.line_count())
                else {
                    continue;
                };
                let tested = tests.tested_by(frag, state, by_id, &link);
                c.file_tested = tested.is_empty() && tests.file_tested(frag, &reaches_file);
                c.tests_open = tested.is_empty() && tests.open_above(frag, &link, true);
                c.tests_partial =
                    !tested.is_empty() && tests.nearest_only(frag, state, by_id, &link);
                c.tests = tested;
            }
            if let Some(c) =
                contract.filter(|c| !diffctx_base::testfiles::is_test_path(Path::new(&c.path)))
            {
                found.contracts.push(c);
            }
            // Unresolved is an answer too: dropped, it read as no caller.
            let alone = sym
                .symbol
                .as_ref()
                .is_some_and(|name| listed_alone.contains(&(sym.path.clone(), name.clone())));
            if !sym.callers.is_empty() || sym.unresolved.is_some() || alone {
                found.changed.push(sym);
            }
        }
    }
    drop(removed);
}
