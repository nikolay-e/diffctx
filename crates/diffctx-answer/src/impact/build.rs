//! The impact answer of a changed range: every changed definition with its callers, tests, removals and
//! unknowns.

use std::time::Instant;

use crate::decide::cells::Cells;

use crate::run_provenance::state::ScoredState;

use diffctx_base::types::Fragment;

use super::tests_reach::TestsReach;

use super::MAX_BLAMED_SYMBOLS;
use super::changes::commits_touching;
use super::output::ImpactOutput;

mod apart;
mod claim;
mod context;
mod contract;
mod definition;
mod gaps;
mod range;
mod removals;
mod role;
mod rows;
mod sources;
mod targets;

use claim::Dossier;
use context::{Findings, Impact};
use sources::Sources;

/// `deadline`: the line log (one git call per changed symbol) runs only
/// while there is time left; commit overlap is the least actionable of the
/// three signals and the first to yield.
pub fn build_impact(
    state: &ScoredState,
    diff_range: Option<&str>,
    query: Option<&str>,
    deadline: Option<Instant>,
) -> ImpactOutput {
    let setup = diffctx_base::profile::phase("impact.setup");
    // The cells: changed definitions from the facts layer's semantic changes,
    // callers from the resolver of their language (decide.rs). The ranking
    // graph and the text filters take no part in what they answer.
    let cells = {
        let _t = diffctx_base::profile::detail("impact.setup.cells", "");
        Cells::open(
            state,
            state.analysed_range.as_deref().or(diff_range),
            query.is_some(),
        )
    };
    let tests = {
        let _t = diffctx_base::profile::detail("impact.setup.tests", "");
        TestsReach::build(
            state,
            &cells,
            state.analysed_range.as_deref().or(diff_range),
        )
    };
    let cx = Impact::open(state, &cells, tests, diff_range, query, deadline);
    let mut found = Findings::default();
    let t = targets::read(&cx, &mut found);
    drop(setup);
    let (state, cores, bounds) = (cx.state, &t.cores, &cx.bounds);
    diffctx_base::profile::count("impact.cores", cores.len() as u64);
    let stopped_early = !state.run.stops().is_empty();
    let mut unreached: Vec<&Fragment> = t.cell_lost.clone();
    let callers = diffctx_base::profile::phase("impact.callers");
    for (i, core) in cores.iter().enumerate() {
        // Past the deadline the symbols already answered are the answer:
        // `limits` then says it is partial, and a mass reformat returns what
        // it found instead of nothing (#382). The definitions it did not
        // reach are named below, never left out of the count as if nothing
        // called them.
        if !state.run.check() {
            unreached.extend(cores[i..].iter().copied());
            break;
        }
        let Some(c) = definition::read(&cx, &t, core, &mut found) else {
            continue;
        };
        let Sources {
            sources,
            also_changed,
            possibly_also_changed,
            loose,
            called_in_diff,
            entered,
        } = sources::of(&cx, &t, &c);
        let callers = rows::of(&cx, &c, sources, loose);
        let core = c.core;
        let in_time = cx.deadline.is_none_or(|d| Instant::now() < d);
        let commits = match (&bounds, i < MAX_BLAMED_SYMBOLS && in_time) {
            (Some((base, head)), true) => {
                let _t = diffctx_base::profile::detail("impact.callers.commits", "");
                commits_touching(state, base, head, core)
            }
            _ => 0,
        };
        contract::note_public_api(&cx, &c, &callers, &mut found);
        let role = role::of(&cx, &c, &callers, &also_changed, &entered);
        claim::settle(
            &cx,
            &t,
            &c,
            Dossier {
                callers,
                commits,
                also_changed,
                possibly_also_changed,
                called_in_diff,
                role,
            },
            &mut found,
        );
    }
    drop(callers);
    gaps::unread(&cx, &t, &mut found);
    let apart_unreached = apart::trace(&cx, &t, &mut found);
    gaps::unfinished(
        &cx,
        &t,
        apart_unreached,
        unreached,
        stopped_early,
        &mut found,
    );
    removals::read(&cx, &t, &mut found);
    range::findings(&cx, &t, found)
}
