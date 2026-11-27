//! The caller rows of a changed definition: one per fragment and kind of use, with their test links.

use rustc_hash::{FxHashMap, FxHashSet};

use diffctx_base::container::innermost;

use diffctx_base::types::{Fragment, FragmentId};

use super::super::callers::first_naming_line;
use super::super::cells::Reach;
use super::super::changes::{lines_of, rel};
use super::super::dispatch::{base_symbol, merge_by_symbol};
use super::super::output::Caller;

use super::context::Impact;
use super::definition::Core;

pub(super) fn of<'a>(
    cx: &Impact<'a>,
    c: &Core<'a, '_>,
    sources: Vec<(&'a Fragment, f64, Option<Reach>)>,
    loose: Vec<Caller>,
) -> Vec<Caller> {
    let state = cx.state;
    let test_links = |frag, core: Option<&Fragment>| cx.test_links(frag, core);
    let core = c.core;
    let symbol = c.symbol;
    let mut sources = sources;
    let weights: FxHashMap<&FragmentId, f64> =
        sources.iter().map(|(f, w, _)| (&f.id, *w)).collect();
    let findings: FxHashMap<&FragmentId, Reach> = sources
        .iter()
        .filter_map(|(f, _, finding)| finding.clone().map(|x| (&f.id, x)))
        .collect();
    // A fragment that reads the definition one way and calls it another
    // has a row for each kind the cell states; with no kind stated it is
    // one row, as the finding above says.
    let mut other_kinds: FxHashMap<&FragmentId, Vec<(&'static str, Reach)>> = FxHashMap::default();
    for (frag, _, finding) in &sources {
        let Some((finding, kind)) = finding
            .as_ref()
            .and_then(|finding| Some((finding, finding.stated_kind(&rel(state, frag.path()))?)))
        else {
            continue;
        };
        let rows = other_kinds.entry(&frag.id).or_default();
        match rows.iter_mut().find(|(stated, _)| *stated == kind) {
            Some(row) => row.1 = finding.clone(),
            None => rows.push((kind, finding.clone())),
        }
    }
    // A name match links an import line too, at the import weight; the
    // function that makes the call is the caller, the import is how it got
    // the name. A binding finding skips import lines, so the module-level
    // code it names calls the definition beside that function (#449).
    let named_files: FxHashSet<&str> = sources
        .iter()
        .filter(|(f, _, _)| f.symbol_name.is_some())
        .map(|(f, _, _)| f.path())
        .collect();
    sources.retain(|(f, _, found)| {
        f.symbol_name.is_some() || found.is_some() || !named_files.contains(f.path())
    });
    let rows = diffctx_base::profile::detail("impact.callers.rows", "");
    let callers: Vec<Caller> = innermost(sources.iter().map(|(f, _, _)| *f).collect())
        .into_iter()
        .flat_map(|frag| {
            let found = findings.get(&frag.id);
            let row = Caller {
                path: rel(state, frag.path()),
                symbol: frag.symbol_name.clone(),
                lines: lines_of(&frag.id),
                weight: (weights[&frag.id] * 1e3).round() / 1e3,
                evidence: found.map_or("name", Reach::evidence),
                reason: found.and_then(Reach::reason),
                relation: found.map_or("call", |f| f.relation),
                kind: found.and_then(|f| f.stated_kind(&rel(state, frag.path()))),
                folded_by: found.and_then(|f| f.fold).map(|(rule, _, _)| rule),
                fold_kind: found.and_then(|f| f.fold).map(|(_, kind, _)| kind),
                fold_share: found.and_then(|f| f.fold).map_or(0, |(_, _, share)| share),
                // A word match cites the line that spells the name, not
                // the start of the function around it.
                sites: match found {
                    Some(f) => f.lines.clone(),
                    None => symbol
                        .and_then(|name| first_naming_line(frag, base_symbol(name)))
                        .into_iter()
                        .collect(),
                },
                ..test_links(frag, Some(core))
            };
            let rest: Vec<Caller> = other_kinds
                .get(&frag.id)
                .into_iter()
                .flatten()
                .filter(|(kind, _)| Some(*kind) != row.kind)
                .map(|(kind, finding)| Caller {
                    evidence: finding.evidence(),
                    reason: finding.reason(),
                    relation: finding.relation,
                    kind: Some(*kind),
                    folded_by: finding.fold.map(|(rule, _, _)| rule),
                    fold_kind: finding.fold.map(|(_, kind, _)| kind),
                    fold_share: finding.fold.map_or(0, |(_, _, share)| share),
                    sites: finding.lines.clone(),
                    ..row.clone()
                })
                .collect();
            std::iter::once(row).chain(rest)
        })
        .collect();
    drop(rows);
    let mut callers = callers;
    callers.extend(loose);
    let mut callers = merge_by_symbol(callers);
    callers.sort_by(|a, b| {
        b.weight
            .total_cmp(&a.weight)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.lines.cmp(&b.lines))
    });
    callers
}
