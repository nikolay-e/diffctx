//! The header, the tail and the sentences of an impact answer's Markdown, over the v2
//! document alone: a writer takes an [`Answer`], which holds no internal answer, so
//! a number printed is a [`Tally`] field, a ledger of the document or a [`Page`]
//! of one list (gh#721).
#![deny(clippy::disallowed_methods)]

use std::fmt::Write as _;

use super::v2::{DefRef, Gap, GuardState, How, ImpactV2, Listing, Range, Standing, Unreached};
use super::{Answer, Page};
use crate::impact::{
    MAX_REMOVED_NAME_HITS, MAX_TEST_HOPS, MAX_TEXTUAL_SHOWN, MAX_UNTESTED_NAMED,
    OUTSIDE_DEFINITIONS, TEXTUAL_MATCH, UNANALYSED_EXAMPLES, base_symbol, reason_names, stops_of,
};
use crate::resource::LimitReason;

/// A definition as the text names it: `path::symbol (lines)`, or `path:lines`.
pub fn place(path: &str, symbol: Option<&str>, lines: &str) -> String {
    match symbol {
        Some(symbol) => format!("{path}::{symbol} ({lines})"),
        None => format!("{path}:{lines}"),
    }
}

pub fn label(def: &DefRef) -> String {
    place(&def.path, def.symbol.as_deref(), &def.lines)
}

/// A definition listed apart, with its role when it has one.
fn unreached(item: &Unreached) -> String {
    match &item.role {
        Some(role) => format!("{} \u{2014} {role}", label(&item.def)),
        None => label(&item.def),
    }
}

/// What a gap says of a definition whose callers are not resolved. The wording of
/// the answer's own reasons (a name on too many lines, reads through a receiver
/// nothing types, a member of a trait) is theirs; any other gap is a limit of the
/// search, said as one.
pub fn gap_sentence(gap: &Gap) -> String {
    let limit = |gap: &Gap| {
        format!("callers not determined: {gap}, so finding none does not mean there are none")
    };
    match gap {
        Gap::Unlanded { name, uses } if *uses > MAX_REMOVED_NAME_HITS => {
            format!(
                "`{name}` is a whole word on {uses} or more lines of code; too common to trace by name"
            )
        }
        Gap::Unlanded { name, uses } => {
            format!("{uses} read(s) of `.{name}` through a receiver the binding model cannot type")
        }
        Gap::Unpaired { name } => format!(
            "`{name}` has more than one counterpart on the other side of the range, so what changed in it is not determined and finding no callers does not mean there are none"
        ),
        Gap::Stopped { reasons } => format!(
            "callers not determined: the run stopped ({}) before the search for this definition finished, so the callers listed are a lower bound and finding none does not mean there are none",
            reason_names(reasons)
        ),
        Gap::Dispatched { via } => format!(
            "implements `{via}`; calls through `dyn {via}` or a generic bound are not resolved"
        ),
        Gap::Ambiguous {
            name,
            definitions,
            language,
        } => format!(
            "`{name}` is ambiguous: {definitions} {language} definitions and no binding model to tell them apart, so callers outside its own file are not resolved"
        ),
        Gap::NotModelled { language } => format!(
            "no binding model reads {language}: calls in its own file and through its types are not traced, so finding none does not mean there are none"
        ),
        Gap::Overridable { via } => {
            format!("calls through {via} are not traced, and no direct caller was found")
        }
        Gap::Untracked {
            name: Some(name), ..
        } => format!(
            "`{name}` is not a tracked definition here (a block-scoped binding); its uses are not resolved"
        ),
        other => limit(other),
    }
}

/// The range as the header says it.
fn range_label(range: &Range) -> String {
    let tree = |tree: &str| tree.chars().take(10).collect::<String>();
    match (range.spec.as_deref(), range.index_tree.as_deref()) {
        // `A..T` names a tree its caller built — the hook's set of edited files —
        // which is no more the index than any other tree (#463).
        (Some(spec), Some(t)) if !crate::snapshot::is_staged_keyword(spec) => {
            format!("the snapshot tree {}", tree(t))
        }
        (_, Some(t)) => format!("staged changes (index tree {})", tree(t)),
        (None | Some("HEAD"), None) => "uncommitted changes".to_string(),
        (Some(spec), None) => spec.to_string(),
    }
}

/// A heading and its first `shown` lines; the rest, `omitted` included, is a
/// count. The heading prints whenever something is counted, so an omitted count
/// never sits under another list's heading (#451).
fn list(out: &mut String, heading: &str, lines: &[String], shown: usize, omitted: usize) {
    let page = Page::of(lines, shown, omitted);
    if page.total == 0 {
        return;
    }
    let _ = writeln!(out, "{heading}");
    for line in page.shown {
        let _ = writeln!(out, "  - {line}");
    }
    if page.rest() > 0 {
        let _ = writeln!(out, "  - and {} more", page.rest());
    }
}

/// The lines of a listing, formatted, with what it omits.
fn listed<T>(
    out: &mut String,
    heading: &str,
    listing: &Listing<T>,
    shown: usize,
    line: impl Fn(&T) -> String,
) {
    let lines: Vec<String> = listing.items.iter().map(line).collect();
    list(out, heading, &lines, shown, listing.omitted());
}

/// The caller count as far as it is known: symbols whose callers could not be
/// resolved, or a run that stopped early, make a zero no finding (#387).
fn callers_phrase(a: &Answer, scope: &str) -> String {
    let doc = a.doc;
    let tally = &a.tally;
    let callers = tally.callers.n;
    // A folded call is still a caller: the count names the share the change cannot
    // have affected.
    let folded_note = if tally.folded_callers > 0 {
        format!(" ({} folded as not affected)", tally.folded_callers)
    } else {
        String::new()
    };
    // Nothing was traced: a count of zero would count what no one looked for.
    if doc.summary.traced == 0
        && !doc.unlisted.layout_only.items.is_empty()
        && tally.listed == 0
        && doc.coverage.limits.is_empty()
    {
        return "callers not traced: only comments, formatting or position changed".to_string();
    }
    let stops: Vec<LimitReason> = stops_of(&doc.coverage.limits);
    let unresolved: Vec<&str> = doc
        .definitions
        .iter()
        .filter(|d| {
            d.gaps
                .iter()
                .any(|g| !matches!(g, Gap::Unaccounted { .. } | Gap::Stopped { .. }))
                && !d.gaps.iter().any(|g| matches!(g, Gap::Stopped { .. }))
        })
        .map(|d| d.symbol.as_deref().unwrap_or(&d.path))
        .collect();
    let unresolved_names = {
        let page = Page::of(&unresolved, 3, 0);
        let shown: Vec<String> = page.shown.iter().map(|n| format!("`{n}`")).collect();
        match page.rest() {
            0 => shown.join(", "),
            more => format!("{} and {more} more", shown.join(", ")),
        }
    };
    let mut phrase = if !stops.is_empty() {
        // A run that stopped has counted what it reached, not what there is: the
        // count is a lower bound and the definitions it never finished are named as
        // such, never counted as callers-free.
        let mut p = match callers {
            0 => format!(
                "no caller found before the run stopped ({})",
                reason_names(&stops)
            ),
            n => format!(
                "{n} caller(s){scope}{folded_note} found before the run stopped ({})",
                reason_names(&stops)
            ),
        };
        if tally.undetermined > 0 {
            let _ = write!(
                p,
                ", callers not determined for {} changed definition(s)",
                tally.undetermined
            );
        }
        if !unresolved.is_empty() {
            let _ = write!(p, ", callers unresolved for {unresolved_names}");
        }
        p
    } else if callers == 0
        && !unresolved.is_empty()
        && Page::of(&unresolved, 0, 0).total == tally.listed
    {
        format!("callers unresolved for {unresolved_names}")
    } else {
        // A count of nothing is an absence: said as one only with the scope whole.
        let mut p = match (tally.callers.exact, callers) {
            (false, 0) => format!("no caller found{scope} in the analysed scope"),
            // A count without its witness is the least there are: said as that.
            (false, n) => format!("at least {n} caller(s){scope}{folded_note}"),
            (true, n) => format!("{n} caller(s){scope}{folded_note}"),
        };
        if !unresolved.is_empty() {
            let _ = write!(p, ", callers unresolved for {unresolved_names}");
        }
        p
    };
    // Word matches are no callers, and no zero either: they are counted where the
    // reader looks first (R1).
    if tally.textual > 0 {
        phrase.push_str(&format!(
            ", {} named only textually (not resolved)",
            tally.textual
        ));
    }
    if stops.is_empty() && !doc.coverage.limits.is_empty() && callers > 0 {
        phrase.push_str(" found in a partial run");
    }
    phrase
}

/// Which changed files were read as text and why: their callers, the definitions in
/// them and the contracts they move are all missing from the counts above, and the
/// reader has to know that to read a zero (#454).
pub fn unanalysed(out: &mut String, doc: &ImpactV2) {
    let files = &doc.coverage.unanalysed;
    let partial = |f: &&crate::impact::UnanalysedFile| f.reason == "parse_partial";
    let untraced = |f: &&crate::impact::UnanalysedFile| f.reason == "content_untraced";
    let outside = |f: &&crate::impact::UnanalysedFile| f.reason == OUTSIDE_DEFINITIONS;
    let line = |group: Vec<&crate::impact::UnanalysedFile>, lead: &str, more: usize| {
        if group.is_empty() {
            return None;
        }
        let mut groups: Vec<(Option<&str>, &str, Vec<String>)> = Vec::new();
        for f in group {
            let shown = match f.lines.first() {
                Some(l) if f.reason == "parse_partial" => format!("{}:{l}", f.path),
                _ => f.path.clone(),
            };
            match groups
                .iter_mut()
                .find(|(l, r, _)| *l == f.language && *r == f.reason)
            {
                Some((_, _, paths)) => paths.push(shown),
                None => groups.push((f.language, f.reason, vec![shown])),
            }
        }
        let groups: Vec<String> = groups
            .into_iter()
            .map(|(language, reason, paths)| {
                let why = crate::parsers::unanalysed_text(reason);
                let page = Page::of(&paths, UNANALYSED_EXAMPLES, 0);
                let examples = page.shown.join(", ");
                let rest = if page.rest() > 0 { ", …" } else { "" };
                let why = if reason == "content_untraced" {
                    String::new()
                } else {
                    format!(" ({why})")
                };
                // A file outside code with no language is no code (#626).
                let kind = if reason == "content_untraced" {
                    "other"
                } else {
                    "code"
                };
                format!(
                    "{} {}{why}: {examples}{rest}",
                    page.total,
                    language.unwrap_or(kind),
                )
            })
            .collect();
        let more = if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        };
        Some(format!("({lead}: {}{more})", groups.join("; ")))
    };
    let whole: Vec<_> = files
        .items
        .iter()
        .filter(|f| !partial(f) && !untraced(f) && !outside(f))
        .collect();
    let spans: Vec<String> = files
        .items
        .iter()
        .filter(outside)
        .map(|f| {
            let runs: Vec<String> = f
                .lines
                .chunks(2)
                .map(|run| format!("{}–{}", run[0], run.last().copied().unwrap_or(run[0])))
                .collect();
            format!("{}:{}", f.path, runs.join(", "))
        })
        .collect();
    if !spans.is_empty() {
        let _ = writeln!(
            out,
            "(changed code outside any tracked definition, whose effect is not traced: {})",
            spans.join("; ")
        );
    }
    let parts: Vec<_> = files.items.iter().filter(partial).collect();
    let content: Vec<_> = files.items.iter().filter(untraced).collect();
    for text in [
        line(
            whole,
            "not analysed — definitions, callers and tests in them are not counted",
            files.omitted() - doc.coverage.untraced_omitted,
        ),
        line(
            parts,
            "analysed only in part — what sits on the lines the parse broke on is not counted",
            0,
        ),
        line(
            content,
            "not followed — only lines naming these files by path were searched; search for the keys and values the change touched",
            doc.coverage.untraced_omitted,
        ),
    ]
    .into_iter()
    .flatten()
    {
        let _ = writeln!(out, "{text}");
    }
    if doc.coverage.keys_unsearched > 0 {
        let _ = writeln!(
            out,
            "(key search capped: the keys and values of {} changed config(s) were not searched)",
            doc.coverage.keys_unsearched
        );
    }
}

/// The first line, and what the run left unread or partial.
pub fn header(out: &mut String, a: &Answer) {
    let doc = a.doc;
    let tally = &a.tally;
    let range = a
        .ctx
        .label
        .clone()
        .unwrap_or_else(|| range_label(&doc.range));
    let callers = tally.callers.n;
    let possible = tally.possible;
    // Labelled, and only beside a caller: "0 without a static test link" next to
    // the definitions' count read as both 0 and N untested (#493).
    let mut guard_summary = if doc.tests.known && callers > 0 {
        format!(
            ", {} caller(s) without a static test link",
            tally.untested_callers
        )
    } else {
        String::new()
    };
    if possible > 0 {
        guard_summary.push_str(&format!(", {possible} possible"));
    }
    if doc.tests.known && doc.tests.definitions_without_link.n > 0 {
        guard_summary.push_str(&format!(
            ", {} changed definition(s) without a static test link",
            doc.tests.definitions_without_link.n
        ));
    }
    // Withheld files matter where they could hide the answer: a changed file, or a
    // symbol with no caller yet. A symbol that has callers is answered, and the
    // count on every lookup taught skipping (#656).
    let withheld_count = if doc.symbol.is_some() && callers + possible > 0 {
        0
    } else {
        doc.inventory.withheld
    };
    let withheld = |what: &str| {
        let mut clause = String::new();
        if withheld_count > 0 {
            clause = format!(", {withheld_count} {what} withheld by ignore rules");
        }
        if tally.not_analysed > 0 {
            clause.push_str(&format!(", {} not analysed", tally.not_analysed));
        }
        if tally.outside_definitions > 0 {
            clause.push_str(&format!(
                ", {} with changed code outside any tracked definition",
                tally.outside_definitions
            ));
        }
        if tally.analysed_in_part > 0 {
            clause.push_str(&format!(
                ", {} analysed only in part",
                tally.analysed_in_part
            ));
        }
        if tally.not_followed > 0 {
            clause.push_str(&format!(", {} not followed", tally.not_followed));
        }
        clause
    };
    match &doc.symbol {
        Some(symbol) => {
            let _ = writeln!(
                out,
                "diffctx impact for symbol {symbol}: {} definition(s){}, {}{guard_summary}",
                tally.definitions,
                withheld("file(s) naming it"),
                callers_phrase(a, ""),
            );
        }
        None => {
            let _ = writeln!(
                out,
                "diffctx impact for {range}: {} changed file(s){}{}{}, {}{}{guard_summary}",
                tally.changed_files,
                if tally.deleted_files == 0 {
                    String::new()
                } else {
                    format!(" ({} deleted)", tally.deleted_files)
                },
                match tally.untracked {
                    0 => String::new(),
                    n => format!(", {n} untracked"),
                },
                withheld("file(s)"),
                callers_phrase(
                    a,
                    if doc.coverage.unanalysed.items.is_empty() {
                        " outside the diff"
                    } else {
                        " outside the diff in the analysed files"
                    }
                ),
                match tally.collection_reads {
                    0 => String::new(),
                    n => format!(", {n} read(s) by collection name"),
                },
            );
        }
    }
    unanalysed(out, doc);
    if withheld_count > 0 {
        let _ = writeln!(
            out,
            "(the withheld files were not read: calls and tests in them are not counted)"
        );
    }
    if !doc.coverage.limits.is_empty() {
        let _ = writeln!(
            out,
            "(partial: {}; callers may be missing)",
            reason_names(&doc.coverage.limits)
        );
    }
    for gap in &doc.coverage.scope.gaps {
        if matches!(gap, Gap::UnreadDirectories { .. }) {
            let _ = writeln!(out, "({gap}: callers in them are not counted)");
        }
    }
}

/// Definitions only comments, formatting or position changed.
pub fn layout_only(out: &mut String, a: &Answer) {
    listed(
        out,
        "Changed only in comments, formatting or position, which no caller sees (callers not traced):",
        &a.doc.unlisted.layout_only,
        MAX_UNTESTED_NAMED,
        label,
    );
}

fn untested(out: &mut String, a: &Answer) {
    let doc = a.doc;
    if !doc.tests.known {
        return;
    }
    // Without a complete scope, no caller found is all the run can say.
    let uncalled = if doc.coverage.scope.complete {
        "Changed, nothing calls them, no static test link:".to_string()
    } else {
        let why: Vec<String> = doc
            .coverage
            .scope
            .gaps
            .iter()
            .map(ToString::to_string)
            .collect();
        format!(
            "Changed, no caller found in the analysed scope ({}), no static test link:",
            why.join("; ")
        )
    };
    listed(
        out,
        &uncalled,
        &doc.unlisted.no_caller,
        MAX_UNTESTED_NAMED,
        unreached,
    );
    listed(
        out,
        "Changed, called only from inside the diff, no static test link:",
        &doc.unlisted.called_in_diff,
        MAX_UNTESTED_NAMED,
        unreached,
    );
}

/// The definitions nothing calls and the ones a runner or a framework enters.
pub fn untested_unlisted(out: &mut String, a: &Answer) {
    untested(out, a);
    listed(
        out,
        "Changed, entered by a runner or the framework, not by a caller:",
        &a.doc.unlisted.entered_by,
        MAX_UNTESTED_NAMED,
        |e| format!("{} \u{2014} {}", label(&e.def), e.role),
    );
}

/// Word matches, listed as text and never counted as callers (R1).
fn textual_rows(doc: &ImpactV2) -> Vec<String> {
    let mut rows = Vec::new();
    for d in &doc.definitions {
        let named = d.symbol.as_deref().map_or(d.path.as_str(), base_symbol);
        for r in d.rows.iter().filter(|r| r.standing == Standing::Name) {
            let at = r.sites.first().map_or(String::new(), |l| format!(":{l}"));
            rows.push(format!("{}{at} names {named}", r.path));
        }
        for f in &d.missed_in {
            let at = f.lines.first().map_or(String::new(), |l| format!(":{l}"));
            rows.push(format!(
                "{}{at} names {named} ({})",
                f.path,
                crate::parsers::unanalysed_text(f.reason)
            ));
        }
    }
    rows
}

/// What is left to say after the entries: the files, the lists and the notes.
pub fn tail(out: &mut String, a: &Answer) {
    let doc = a.doc;
    listed(
        out,
        "Lock files changed (dependency versions moved; what that reaches is not traced):",
        &doc.inventory.lockfiles,
        usize::MAX,
        Clone::clone,
    );
    layout_only(out, a);
    untested_unlisted(out, a);
    listed(
        out,
        "Still naming a deleted or renamed path:",
        &doc.stale,
        usize::MAX,
        |r| {
            let kind = match (r.how, &r.to) {
                (How::Renamed, Some(to)) => format!("renamed to {to}"),
                _ => "deleted".to_string(),
            };
            format!(
                "{}:{} names {} ({kind}) [{}]",
                r.path,
                r.line,
                r.target,
                r.class.name()
            )
        },
    );
    list(
        out,
        &format!("Named in code outside the diff ({TEXTUAL_MATCH}):"),
        &textual_rows(doc),
        MAX_TEXTUAL_SHOWN,
        a.tally.shed_textual,
    );
    listed(
        out,
        "Named by path outside the diff (a command line, a config, another runtime):",
        &doc.named.by_path,
        usize::MAX,
        |r| {
            format!(
                "{}:{} names {} [{}]",
                r.path,
                r.line,
                r.target,
                r.class.name()
            )
        },
    );
    listed(
        out,
        &format!("Named by a changed key or value outside the diff ({TEXTUAL_MATCH}):"),
        &doc.named.by_key,
        usize::MAX,
        |r| {
            format!(
                "{}:{} names {} ({})",
                r.path,
                r.line,
                r.key.as_deref().unwrap_or(""),
                r.target
            )
        },
    );
    let unnamed: Vec<String> = doc
        .named
        .key_searches
        .iter()
        .filter(|k| k.named == 0)
        .map(|k| format!("{} ({})", k.path, k.keys.join(", ")))
        .collect();
    if !unnamed.is_empty() {
        let _ = writeln!(
            out,
            "(changed keys and values searched as text, named nowhere outside the diff: {})",
            unnamed.join("; ")
        );
    }
    listed(
        out,
        "Read by collection name outside the diff (Astro content collections):",
        &doc.named.collections,
        usize::MAX,
        |r| match &r.collection {
            Some(c) => format!("{}:{} reads collection {c}", r.path, r.line),
            None => format!("{}:{} reads a collection named at run time", r.path, r.line),
        },
    );
    listed(
        out,
        "Contracts changed:",
        &doc.contracts,
        usize::MAX,
        |c| match &c.symbol {
            Some(s) => format!("{} {}::{s}", c.kind, c.path),
            None => format!("{} {}", c.kind, c.path),
        },
    );
    // The omitted entry says how many callers it carries and its own budget.
    if a.tally.shed_from_listed > 0 {
        let _ = writeln!(
            out,
            "({} weaker caller(s) omitted to stay under {} tokens; a larger budget (--budget, budget_tokens) lists them)",
            a.tally.shed_from_listed, doc.fit.cap
        );
    }
    if doc
        .definitions
        .iter()
        .flat_map(|d| &d.rows)
        .any(|r| r.guard.state == GuardState::Linked && r.standing != Standing::Name)
    {
        let _ = writeln!(
            out,
            "Test links are static references within {MAX_TEST_HOPS} call hops, not execution coverage."
        );
    }
}
