//! Definitions the answer names without tracing them: a variable no fact stands for, a file its cell lost,
//! what an early stop never reached.

use rustc_hash::FxHashSet;

use crate::decide::{self};

use diffctx_base::types::Fragment;

use super::super::callers::is_code_file;
use super::super::changes::{changed_rels, layout_files, lines_of, moved_verbatim, rel};
use super::super::coverage::{uncovered_symbol, unfinished_symbol};
use super::super::dispatch::base_symbol;
use super::super::output::{ChangedSymbol, Unknown};

use super::context::{Findings, Impact};
use super::targets::Targets;

/// A queried name no cell read, and the definitions of files a cell lost: said, not left out.
pub(super) fn unread(cx: &Impact<'_>, t: &Targets<'_>, found: &mut Findings) {
    let (state, query, cells) = (cx.state, cx.query, cx.cells);
    let (cores, cell_defs, cell_apart, cell_removed, cell_unread) = (
        &t.cores,
        &t.cell_defs,
        &t.cell_apart,
        &t.cell_removed,
        &t.cell_unread,
    );
    let mut cell_untracked = t.cell_untracked.clone();
    cell_untracked.sort_by(|a, b| (a.path(), a.start_line()).cmp(&(b.path(), b.start_line())));
    cell_untracked.dedup_by(|a, b| a.path() == b.path() && a.start_line() == b.start_line());
    for core in cell_untracked {
        let name = core.symbol_name.as_deref().map_or("", base_symbol);
        found.changed.push(ChangedSymbol {
            unresolved: Some(Unknown::Untracked {
                name: name.to_string(),
            }),
            ..ChangedSymbol::bare(
                rel(state, core.path()),
                Some(name.to_string()),
                lines_of(&core.id),
                "variable",
                decide::coverage::Coverages::partial(decide::gap::Gap::NotTraced),
            )
        });
    }
    // A name asked for that no cell read a definition of, in a group whose file
    // was lost where the name is spelled: the loss is the answer, not an empty one.
    if let Some(asked) = query.filter(|_| {
        cell_defs.is_empty()
            && cell_apart.is_empty()
            && cell_removed.is_empty()
            && cell_unread.is_empty()
            && cores.is_empty()
    }) {
        let name = asked.rsplit(':').next().unwrap_or(asked);
        for (path, reason) in cells.iter().flat_map(|cell| cell.lost_namers(name)) {
            found.changed.push(uncovered_symbol(
                path.as_str().to_string(),
                name,
                String::new(),
                "function".to_string(),
                reason,
            ));
        }
    }
    for &(core, reason) in cell_unread {
        found.changed.push(uncovered_symbol(
            rel(state, core.path()),
            core.symbol_name.as_deref().map_or("", base_symbol),
            lines_of(&core.id),
            format!("{:?}", core.kind).to_lowercase(),
            reason,
        ));
    }
}

/// What the deadline or a limit left untraced: the definitions never reached, and the changed code files
/// a run already stopped before the callers phase may have lost fragments of.
pub(super) fn unfinished(
    cx: &Impact<'_>,
    t: &Targets<'_>,
    apart_unreached: Vec<&decide::changed::Changed>,
    unreached: Vec<&Fragment>,
    stopped_early: bool,
    found: &mut Findings,
) {
    let (state, query) = (cx.state, cx.query);
    let (cores, bordering) = (&t.cores, &t.bordering);
    let hunks_in = |core: &Fragment| cx.hunks_in(core);
    {
        let stops = state.run.stops();
        for entry in apart_unreached {
            found.changed.push(unfinished_symbol(
                entry.file.clone(),
                entry.symbol(),
                format!("{}-{}", entry.lines.0, entry.lines.1),
                format!("{:?}", entry.kind).to_lowercase(),
                &stops,
            ));
        }
        for core in unreached {
            let hunks = hunks_in(core);
            let skipped = query.is_none()
                && (bordering.contains(&core.id)
                    || (!hunks.is_empty()
                        && hunks
                            .iter()
                            .all(|h| h.layout || moved_verbatim(h, &state.hunk_texts))));
            if !skipped {
                found.changed.push(unfinished_symbol(
                    rel(state, core.path()),
                    core.symbol_name.clone(),
                    lines_of(&core.id),
                    format!("{:?}", core.kind).to_lowercase(),
                    &stops,
                ));
            }
        }
        // A run already stopped when the callers phase began may have lost
        // fragments before it: a changed code file with no definition in
        // view is not known to have none. A file renamed byte for byte or only
        // relaid changed none.
        if stopped_early && query.is_none() {
            let covered: FxHashSet<String> = cores.iter().map(|c| rel(state, c.path())).collect();
            let unseen = layout_files(state);
            for path in changed_rels(state)
                .filter(|p| is_code_file(p) && !covered.contains(p) && !unseen.contains(p.as_str()))
            {
                found.changed.push(unfinished_symbol(
                    path,
                    None,
                    String::new(),
                    "file".to_string(),
                    &stops,
                ));
            }
        }
    }
}
