//! Delivering an impact answer inside a token cap: the v2 document, what is shed or folded to fit it, and
//! the automatic rendering.

use std::path::Path;

use crate::answer::render::place;

use crate::answer::{Answer, RenderCtx};

use crate::decide::{self};

use super::text::render;

use super::MAX_CHANGED_FILES_LISTED;
use super::MAX_TEST_HOPS;
use super::MAX_UNANALYSED_LISTED;
use super::output::{
    Caller, ChangedSymbol, Folded, ImpactOutput, OmittedDefinition, Unknown, is_omitted,
    is_unfinished,
};
use super::symbol::{MAX_UNTESTED_NAMED, is_textual, without_test_link};

/// The answer as JSON: `diffctx.impact.v2`, built from the answer the builders made.
fn v2_json(output: &ImpactOutput) -> String {
    serde_json::to_string(&v2_of(output))
        .unwrap_or_else(|e| format!(r#"{{"error":"the answer could not be serialized: {e}"}}"#))
}

/// The v2 document of an answer: what its text and its JSON both print.
pub fn v2_of(output: &ImpactOutput) -> crate::answer::v2::ImpactV2 {
    crate::answer::v2::ImpactV2::from_v1(output, MAX_TEST_HOPS)
}

/// Drops word matches, then possible callers, then the rows dealt last among
/// the (definition, relation kind) groups of the rest (`last_dealt`), until
/// `measure` fits `cap`, then symbols that have nothing left to say. The cap
/// is on what the reader receives, so it is measured on the delivered text.
fn fit_with(output: &mut ImpactOutput, cap: u32, measure: &dyn Fn(&ImpactOutput) -> u32) {
    output.cap = cap;
    // `empty` is what the text prints a short answer for: it holds for the answer
    // each step measures, as it will for the one delivered.
    output.empty = proves_empty(output);
    // What showing everything would take, to the next hundred tokens.
    output.needs = measure(output).div_ceil(100) * 100;
    let steps = diffctx_base::fit::shed_to_fit(output, cap, measure);
    diffctx_base::profile::count("fit.steps", steps as u64);
}

impl diffctx_base::fit::Shed for ImpactOutput {
    fn shed_one(&mut self) -> bool {
        shed_one(self)
    }

    fn settle(&mut self) -> bool {
        let stopped = self.run.clone().filter(|run| !run.check());
        for reason in stopped
            .iter()
            .flat_map(|run| diffctx_snapshot::resource::classify_limits(run, &[], &|_| false).0)
        {
            if let Err(at) = self.limits.binary_search(&reason) {
                self.limits.insert(at, reason);
            }
        }
        self.empty = proves_empty(self);
        stopped.is_some()
    }
}

/// The one rule for `empty` (#505), read off the answer's v2 document: what its
/// text and its JSON both stand on.
pub(super) fn proves_empty(output: &ImpactOutput) -> bool {
    let doc = v2_of(output);
    Answer::of(&doc, &RenderCtx::default()).proves_empty()
}

/// The hook's one line for the case where the only thing keeping an answer from
/// being `empty` is files the run withheld; `None` for any other.
pub fn unread_line(output: &ImpactOutput, range: &str) -> Option<String> {
    let doc = v2_of(output);
    Answer::of(&doc, &RenderCtx::default())
        .withheld_only()
        .then(|| {
            format!(
                "diffctx: {range} reaches nothing outside the diff in the files it read; not analysed: {} (what they name is unknown).",
                decide::gap::Gap::Withheld {
                    files: doc.inventory.withheld
                }
            )
        })
}

/// One step of shedding, least needed first; `false` once nothing more can
/// go. Callers are the answer, so everything else yields to them first, and
/// the file list — which the reader holds as the diff — first of all.
fn shed_one(output: &mut ImpactOutput) -> bool {
    if output.untracked_files.len() > MAX_CHANGED_FILES_LISTED {
        output.untracked_files_omitted += output.untracked_files.len() - MAX_CHANGED_FILES_LISTED;
        output.untracked_files.truncate(MAX_CHANGED_FILES_LISTED);
    }
    if output.changed_files.len() > MAX_CHANGED_FILES_LISTED {
        output.changed_files_omitted += output.changed_files.len() - MAX_CHANGED_FILES_LISTED;
        output.changed_files.truncate(MAX_CHANGED_FILES_LISTED);
        return true;
    }
    // What names a changed file by path is context: it goes before any
    // caller, a possible one included, and its count line goes with it.
    if let Some(row) = output.named_by.pop() {
        match row.key {
            Some(_) => output.named_by_key_omitted += 1,
            None => output.named_by_path_omitted += 1,
        }
        return true;
    }
    if output.named_by_path_omitted + output.named_by_key_omitted > 0 {
        output.named_by_path_omitted = 0;
        output.named_by_key_omitted = 0;
        return true;
    }
    if output.deleted_modules.pop().is_some() {
        output.deleted_modules_omitted += 1;
        return true;
    }
    if output.layout_only.len() > MAX_UNTESTED_NAMED {
        output.layout_only_omitted += output.layout_only.len() - MAX_UNTESTED_NAMED;
        output.layout_only.truncate(MAX_UNTESTED_NAMED);
        return true;
    }
    if output.unanalysed_files.len() > MAX_UNANALYSED_LISTED {
        output.unanalysed_files_omitted += output.unanalysed_files.len() - MAX_UNANALYSED_LISTED;
        output.untraced_omitted += output.unanalysed_files[MAX_UNANALYSED_LISTED..]
            .iter()
            .filter(|f| f.reason == "content_untraced")
            .count();
        output.unanalysed_files.truncate(MAX_UNANALYSED_LISTED);
        return true;
    }
    // The definitions of test files rank last among the targets: they go first,
    // whole, into the entry that names what the cap shed.
    if let Some(i) = output.changed.iter().rposition(|c| {
        !is_omitted(c)
            && !is_unfinished(c)
            && diffctx_base::testfiles::is_test_path(Path::new(&c.path))
    }) {
        shed_definition(output, i);
        return true;
    }
    // A word match is the weakest row there is: it goes before a guess.
    if let Some((i, r)) = last_dealt(&output.changed, |_, row| is_textual(row)) {
        let row = output.changed[i].callers.remove(r);
        output.changed[i].shed.add(&row);
        output.changed[i].callers_omitted += 1;
        output.truncated += 1;
        output.truncated_textual += 1;
        return true;
    }
    // A possible caller is a guess; a changed contract is a fact (#413).
    if let Some((i, r)) = last_dealt(&output.changed, |_, row| row.evidence == "candidate") {
        let row = output.changed[i].callers.remove(r);
        output.changed[i].shed.add(&row);
        output.changed[i].callers_omitted += 1;
        output.truncated += 1;
        output.truncated_possible += 1;
        return true;
    }
    // The sites a gap names of the uses it cannot settle are the detail of a
    // guess: the count stays, the names go before any caller or definition does.
    if shed_candidate_sites(output) {
        return true;
    }
    // A removed or changed contract is a break the reader must see; only the
    // public API the range merely touches is context (#663). The first stays:
    // a heading over a count alone names no contract (fj#219).
    if output.contracts.len() > 1
        && output
            .contracts
            .last()
            .is_some_and(|c| c.kind == "public_api")
    {
        output.contracts.pop();
        output.contracts_omitted += 1;
        return true;
    }
    // A symbol whose callers could not be resolved says so to the last:
    // dropping it would turn "unknown" into "nothing".
    if let Some(i) = output
        .changed
        .iter()
        .rposition(|c| c.callers.is_empty() && c.unresolved.is_none() && c.compatible_callers == 0)
    {
        omit(output, i);
        return true;
    }
    // A definition the run never searched goes into a count the header keeps. Once
    // the run is over nothing is measured and they all go, as one step each would
    // end: a step per definition rescanned the whole answer for every one of them.
    let never_searched =
        |c: &ChangedSymbol| c.callers.is_empty() && c.compatible_callers == 0 && is_unfinished(c);
    if let Some(i) = output.changed.iter().rposition(never_searched) {
        if output.run.as_ref().is_some_and(|run| run.tripped()) {
            let before = output.changed.len();
            output.changed.retain(|c| !never_searched(c));
            output.unfinished_omitted += before - output.changed.len();
        } else {
            output.changed.remove(i);
            output.unfinished_omitted += 1;
        }
        return true;
    }
    if shed_weakest_caller(output) {
        return true;
    }
    // When even the entry outgrows the cap, its list gives up the names with the
    // fewest callers; the count and the callers stay whole, and the first name,
    // the definition with the most callers, stays: the least the answer can say.
    if let Some(entry) = output.changed.iter_mut().find(|c| is_omitted(c)) {
        if entry.omitted.len() > 1 && entry.omitted.pop().is_some() {
            return true;
        }
    }
    // A file still naming a deleted or renamed path is a defect, not a
    // caller to check: it outlives every caller row, first entries kept
    // (#600).
    if output.stale_references.pop().is_some() {
        output.stale_references_omitted += 1;
        return true;
    }
    // Last, what the counts already say: the header counts the definitions no
    // test reaches, the omitted entry its definitions and callers, so their
    // names go only when nothing else can, and the answer still fits.
    if output.untested_called_in_diff.pop().is_some() || output.untested_unlisted.pop().is_some() {
        return true;
    }
    false
}

/// Whether any definition gave up the candidate sites of a gap that counts more
/// uses than it names.
fn shed_candidate_sites(output: &mut ImpactOutput) -> bool {
    let mut shed = false;
    for sym in &mut output.changed {
        shed |= sym.coverage.shed_sites();
        if let Some(Unknown::Candidates { uses, sites, .. }) = &mut sym.unresolved {
            if *uses > sites.len() && !sites.is_empty() {
                sites.clear();
                shed = true;
            }
        }
    }
    shed
}

/// The shed definitions the omitted entry names; the rest are counted.
const MAX_OMITTED_NAMED: usize = 40;

/// A shed symbol is not dropped: it joins the one entry that names the
/// definitions the cap left out, with the callers it had, and what shows them.
/// Dropped silently it would read as a definition nothing calls.
fn omit(output: &mut ImpactOutput, i: usize) {
    let needs = output.needs;
    let sym = output.changed.remove(i);
    let incomplete = sym.coverage.calls().witness().is_none();
    let gone = OmittedDefinition {
        definition: match &sym.symbol {
            Some(name) => format!("{}::{name}", sym.path),
            None => sym.path.clone(),
        },
        lines: sym.lines.clone(),
        callers_omitted: sym.callers.len() + sym.callers_omitted,
        shed: {
            let mut shed = sym.shed;
            for row in &sym.callers {
                shed.add(row);
            }
            shed
        },
        incomplete,
    };
    // The rows it folded are counted with it: the header still names them.
    let folded = sym.folded;
    match output.changed.iter_mut().find(|c| is_omitted(c)) {
        Some(entry) => {
            entry.folded.extend(folded);
            if let Some(Unknown::Omitted {
                count,
                incomplete: unsearched,
                ..
            }) = &mut entry.unresolved
            {
                *count += 1;
                *unsearched += usize::from(gone.incomplete);
            }
            entry.callers_omitted += gone.callers_omitted;
            entry.shed.merge(gone.shed);
            entry.omitted.push(gone);
            entry.omitted.sort_by(|a, b| {
                b.callers_omitted
                    .cmp(&a.callers_omitted)
                    .then_with(|| a.definition.cmp(&b.definition))
            });
            // The count stays whole; the names that fit are the ones with the
            // most callers, so a module of thirty thousand removals does not
            // outgrow the cap with its own list.
            entry.omitted.truncate(MAX_OMITTED_NAMED);
        }
        None => output.changed.push(ChangedSymbol {
            unresolved: Some(Unknown::Omitted {
                needs,
                count: 1,
                incomplete: usize::from(gone.incomplete),
            }),
            shed: gone.shed,
            callers_omitted: gone.callers_omitted,
            omitted: vec![gone],
            folded,
            ..ChangedSymbol::bare(
                "(changed definitions)".to_string(),
                None,
                String::new(),
                "omitted",
                decide::coverage::Coverages::partial(decide::gap::Gap::Capped),
            )
        }),
    }
}

/// The order a symbol's relation kinds are dealt in and listed.
pub(super) fn relation_rank(relation: &str) -> u8 {
    match relation {
        "call" => 0,
        "implementation" => 1,
        "reference" => 2,
        "reexports" => 3,
        _ => 4,
    }
}

/// The row the cap sheds next among those `accepts` offers, as (symbol, row).
/// Rows are dealt out in rounds over (changed definition, relation kind): each
/// round gives every group that still has rows its next-strongest one, in the
/// order the definitions are listed, and the row dealt last goes first. So the
/// strongest row of every kind every definition has outlasts all the others,
/// no kind is shed before another, and a larger budget shows a superset of a
/// smaller one's rows: the answer is a prefix of one fixed order.
fn last_dealt(
    changed: &[ChangedSymbol],
    accepts: impl Fn(&ChangedSymbol, &Caller) -> bool,
) -> Option<(usize, usize)> {
    let mut last: Option<((usize, usize, u8), (usize, usize))> = None;
    for (i, c) in changed.iter().enumerate() {
        let mut dealt: Vec<(&str, usize)> = Vec::new();
        for (r, row) in c
            .callers
            .iter()
            .enumerate()
            .filter(|(_, row)| accepts(c, row))
        {
            let round = match dealt.iter_mut().find(|(kind, _)| *kind == row.relation) {
                Some((_, rows)) => {
                    *rows += 1;
                    *rows
                }
                None => {
                    dealt.push((row.relation, 0));
                    0
                }
            };
            let order = (round, i, relation_rank(row.relation));
            if last.as_ref().is_none_or(|(latest, _)| order > *latest) {
                last = Some((order, (i, r)));
            }
        }
    }
    last.map(|(_, at)| at)
}

fn shed_weakest_caller(output: &mut ImpactOutput) -> bool {
    // A definition that keeps another row gives up the one dealt last: the
    // strongest row of each relation kind goes only when nothing else in the
    // answer can, and rows of one kind never outlast another kind wholesale,
    // which weights alone did (a symbol read by many and called by few lost
    // its reads altogether).
    if let Some((i, r)) = last_dealt(&output.changed, |c, _| c.callers.len() >= 2) {
        let shed = output.changed[i].callers.remove(r);
        output.changed[i].shed.add(&shed);
        output.changed[i].callers_omitted += 1;
        output.truncated_untested += usize::from(without_test_link(&shed));
        output.truncated += 1;
        return true;
    }
    // A definition with a hedge to say and no caller joins the omitted entry,
    // which keeps that its search was open, before a definition with a caller
    // does: callers are the answer.
    if let Some(i) = output.changed.iter().rposition(|c| {
        c.callers.is_empty()
            && c.compatible_callers == 0
            && c.unresolved
                .as_ref()
                .is_some_and(|_| !is_omitted(c) && !is_unfinished(c))
    }) {
        omit(output, i);
        return true;
    }
    // A symbol's only caller goes last: dropping a whole symbol while another
    // still has a tail to shed would make the selection look arbitrary, one
    // function listed with all its callers and another with none (#386). The
    // strongest caller of a symbol is the fact, so the symbol goes whole once
    // that is all it has left, never one caller at a time.
    match last_dealt(&output.changed, |c, _| {
        c.callers.len() == 1 && c.unresolved.is_none()
    }) {
        Some((i, _)) => {
            shed_definition(output, i);
            true
        }
        None => false,
    }
}

/// A definition shed whole, with the rows it had: they are counted as shed, and
/// the omitted entry names it.
fn shed_definition(output: &mut ImpactOutput, i: usize) {
    let gone = &output.changed[i];
    output.truncated += gone.callers.len();
    output.truncated_possible += gone
        .callers
        .iter()
        .filter(|c| c.evidence == "candidate")
        .count();
    output.truncated_textual += gone.callers.iter().filter(|c| is_textual(c)).count();
    output.truncated_untested += gone.callers.iter().filter(|c| without_test_link(c)).count();
    omit(output, i);
}

/// The answer as the reader receives it, fitted to `cap` tokens measured on
/// that very text. JSON was fitted while markdown was printed, so a 148-token
/// answer cited a 2000-token cap and lost its contracts (#451); `--budget`
/// and `budget_tokens` set the cap, which stayed 2000 whatever was asked
/// (#466). The text a door puts before the answer (the MCP's note that its
/// ceiling lowered the budget) is charged to `cap` with the rest. The returned
/// text is the answer alone: the door puts its frame before it.
pub fn deliver_framed(
    output: &mut ImpactOutput,
    markdown: bool,
    cap: u32,
    unfold: bool,
    frame: &str,
) -> String {
    let _render = diffctx_base::profile::phase("render");
    if !unfold {
        fold_rows(output);
    }
    let markdown_tokens = |o: &ImpactOutput| {
        diffctx_base::tokenizer::count_tokens(&format!("{frame}{}", render(o, false)))
    };
    let json_tokens = |o: &ImpactOutput| {
        diffctx_base::tokenizer::count_tokens(&format!("{frame}{}\n", v2_json(o)))
    };
    let measure: &dyn Fn(&ImpactOutput) -> u32 = if markdown {
        &markdown_tokens
    } else {
        &json_tokens
    };
    fit_with(output, cap, measure);
    // The least the answer can say may outgrow the budget: it names the tokens
    // that take, cutting nothing more, and a budget of that size delivers it.
    // The document prints its cap, so the figure is the one that cap prints
    // itself into, found from a cap of 0 whatever the budget asked.
    if measure(output) > cap {
        let mut least = 0;
        loop {
            output.cap = least;
            let printed = measure(output);
            if printed <= least {
                break;
            }
            least = printed;
        }
        output.cap = cap;
        output.budget_needed = Some(least);
    }
    if markdown {
        render(output, false)
    } else {
        v2_json(output)
    }
}

/// Counts the rows the folding table says the change cannot have affected in a
/// line each, by (relation kind, rule), and takes them out of the listed
/// callers. The count is exact under the definition's `Complete` witness of the
/// relation, otherwise "at least"; the examples are the first three by path and
/// line.
fn fold_rows(output: &mut ImpactOutput) {
    for sym in &mut output.changed {
        if !sym.callers.iter().any(|c| c.folded_by.is_some()) {
            continue;
        }
        let (mut rows, kept): (Vec<Caller>, Vec<Caller>) = std::mem::take(&mut sym.callers)
            .into_iter()
            .partition(|c| c.folded_by.is_some());
        sym.callers = kept;
        // A test is what the reviewer runs, so it is named first; then by path
        // and line, the same for the same change.
        let is_test = |c: &Caller| diffctx_base::testfiles::is_test_path(Path::new(&c.path));
        rows.sort_by(|a, b| {
            (
                !is_test(a),
                a.path.as_str(),
                a.sites.first(),
                a.lines.as_str(),
            )
                .cmp(&(
                    !is_test(b),
                    b.path.as_str(),
                    b.sites.first(),
                    b.lines.as_str(),
                ))
        });
        let mut groups: Vec<(&'static str, &'static str, Vec<Caller>)> = Vec::new();
        for row in rows {
            let (Some(rule), Some(kind)) = (row.folded_by, row.fold_kind) else {
                sym.callers.push(row);
                continue;
            };
            match groups.iter_mut().find(|(r, k, _)| *r == rule && *k == kind) {
                Some((_, _, group)) => group.push(row),
                None => groups.push((rule, kind, vec![row])),
            }
        }
        groups.sort_by_key(|(rule, kind, _)| (*rule, *kind));
        for (rule, kind, group) in groups {
            let coverage = match kind {
                "calls" | "instantiates" | "overrides" => sym.coverage.calls().clone(),
                "reads" | "writes" | "passes_as_value" => sym.coverage.reads().clone(),
                _ => sym.coverage.all(),
            };
            sym.folded.push(Folded {
                rule,
                kind,
                count: group.len(),
                exact: coverage.witness().is_some(),
                examples: group
                    .iter()
                    .take(3)
                    .map(|c| {
                        let line = c
                            .sites
                            .first()
                            .map_or_else(|| c.lines.clone(), |site| format!("{site}-{site}"));
                        place(&c.path, c.symbol.as_deref(), &line)
                    })
                    .collect(),
                parameters: if rule == "check-the-default" {
                    sym.new_parameters.clone()
                } else {
                    Vec::new()
                },
                sharing: group.iter().map(|c| c.fold_share).max().unwrap_or(0),
            });
        }
    }
    output.empty = output.empty && proves_empty(output);
}

/// The automatic answer's size, limits and notes included. An explicit
/// request keeps the engine's own cap.
pub const AUTOMATIC_TOKEN_CAP: u32 = 600;

/// The automatic (hook) answer: candidates counted, not listed, and the
/// whole text, limits and metadata included, within `cap` tokens.
pub fn render_automatic(output: &mut ImpactOutput, cap: u32) -> String {
    fold_rows(output);
    let measure = |o: &ImpactOutput| diffctx_base::tokenizer::count_tokens(&render(o, true));
    fit_with(output, cap, &measure);
    render(output, true)
}
