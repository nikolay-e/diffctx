//! The Markdown text of an impact answer: the entries, over the v2 document alone.
//! The header, the tail and the lists are `crate::answer::render`'s. Nothing here
//! reads the internal answer: the one function that holds it builds the document
//! and hands it, with the presentation parameters, to the writers, so a fact the
//! text prints is a fact the JSON carries (gh#721).
#![deny(clippy::disallowed_methods)]

use std::fmt::Write as _;

use rustc_hash::FxHashMap;

use super::{
    ImpactOutput, MAX_ALSO_CHANGED_SHOWN, MAX_CALLERS_SHOWN_PER_FILE, MAX_CANDIDATES_SHOWN,
    TEXTUAL_MATCH, TOO_COMMON_NAMED, TestLink, base_symbol, reason_names, stops_of, v2_of,
};
use crate::answer::render::{self as rendered, gap_sentence, label, place};
use crate::answer::v2::{
    Class, DefRef, Definition, Fold, Gap, GuardState, LocalOf, RangeKind, Row, Spelled, Standing,
};
use crate::answer::{Answer, Page, RenderCtx};

const NOT_ESTABLISHED: &str = "tests not established";

/// The hook's and the CLI's text form: one line per changed symbol, one per
/// caller beneath it, contracts last. Short enough to read before a commit.
pub fn render_markdown(output: &ImpactOutput) -> String {
    render(output, false)
}

pub(super) fn render(output: &ImpactOutput, compact: bool) -> String {
    let doc = v2_of(output);
    let ctx = RenderCtx {
        label: output.range_name.clone(),
        compact,
    };
    let answer = Answer::of(&doc, &ctx);
    let mut out = String::new();
    write_answer(&mut out, &answer);
    if let Some(needed) = doc.fit.least {
        let _ = writeln!(
            out,
            "(budget too small: the least this answer can say takes {needed} tokens, over the {} asked; `--budget {needed}` or a larger one delivers it without this note)",
            doc.fit.cap
        );
    }
    out
}

fn write_answer(out: &mut String, a: &Answer) {
    let doc = a.doc;
    if let (Some(symbol), Some(spelled)) = (&doc.symbol, &doc.spelled) {
        write_spelled(out, a, symbol, spelled);
        return;
    }
    rendered::header(out, a);
    if (doc.empty || a.withheld_only()) && a.tally.changed_files == 0 && a.tally.untracked == 0 {
        // Every change withheld is not a clean tree: the header already says
        // how many files were withheld and that none was read.
        if doc.inventory.withheld > 0 {
            return;
        }
        // `HEAD` is the uncommitted work, so on a clean tree it reads as
        // "nothing is reached" while the last commit may reach a lot (#368).
        let _ = writeln!(
            out,
            "No changes to analyse.{}",
            if doc.range.kind == RangeKind::Worktree {
                " The working tree matches HEAD; HEAD~1..HEAD is the last commit."
            } else {
                ""
            }
        );
        return;
    }
    let scope = doc.coverage.scope.complete;
    if doc.empty {
        if doc.unlisted.layout_only.items.is_empty() || doc.summary.traced > 0 {
            let _ = writeln!(
                out,
                "No resolved static callers outside the diff in the analysed scope."
            );
        }
        // What the claim stands on is shown: the definitions only the range's own
        // changes use, and the ones it removed that nothing references.
        write_inside_only(out, a);
        write_deleted_modules(out, a);
        for d in doc.definitions.iter().filter(|d| !inside_only(d)) {
            write_definition(out, a, d, scope);
        }
        rendered::layout_only(out, a);
        rendered::untested_unlisted(out, a);
        return;
    }
    // A name defined all over a language without a binding model says the
    // same thing on every row: one line holds them, and the cap goes to the
    // callers that could be resolved (#430).
    let fold_common = doc
        .definitions
        .iter()
        .filter(|d| too_common(d))
        .nth(1)
        .is_some();
    // So do the methods of one trait, each reached through `dyn` (#545).
    let mut traits: Vec<&str> = doc.definitions.iter().filter_map(dispatched).collect();
    traits.sort_unstable();
    let shared: Vec<&str> = traits
        .chunk_by(|a, b| a == b)
        .filter(|members| members.get(1).is_some())
        .map(|members| members[0])
        .collect();
    let folded = |d: &Definition| {
        (fold_common && too_common(d))
            || dispatched(d).is_some_and(|t| shared.contains(&t))
            || stalled(d)
            || inside_only(d)
    };
    for d in doc.definitions.iter().filter(|d| !folded(d)) {
        write_definition(out, a, d, scope);
    }
    write_inside_only(out, a);
    write_deleted_modules(out, a);
    let stalled_here: Vec<&Definition> = doc.definitions.iter().filter(|d| stalled(d)).collect();
    write_unfinished(out, a, &stalled_here);
    write_omitted(out, a);
    if fold_common {
        let common: Vec<&Definition> = doc.definitions.iter().filter(|d| too_common(d)).collect();
        write_too_common(out, &common);
    }
    for t in shared {
        let members: Vec<&Definition> = doc
            .definitions
            .iter()
            .filter(|d| dispatched(d) == Some(t))
            .collect();
        let _ = writeln!(
            out,
            "- {} changed method(s) implementing `{t}` (calls through `dyn {t}` or a generic bound are not resolved): {}",
            Page::of(&members, TOO_COMMON_NAMED, 0).total,
            named_list(&members)
        );
    }
    rendered::tail(out, a);
}

/// A definition with nothing of its own to list: no row, no fold, no file that
/// names it unread, and not touched twice.
fn quiet(d: &Definition) -> bool {
    d.rows.is_empty() && d.folded.is_empty() && d.missed_in.is_empty() && d.commits < 2
}

/// A name defined all over a language without a binding model.
fn too_common(d: &Definition) -> bool {
    quiet(d) && matches!(d.limit(), Some(Gap::Ambiguous { .. }))
}

/// A member of a trait, reached through `dyn` or a bound: the trait's name.
fn dispatched(d: &Definition) -> Option<&str> {
    match d.limit() {
        Some(Gap::Dispatched { via }) if quiet(d) => Some(via.as_str()),
        _ => None,
    }
}

/// A definition the run stopped before it searched has nothing of its own to list.
fn stalled(d: &Definition) -> bool {
    quiet(d) && matches!(d.limit(), Some(Gap::Stopped { .. }))
}

/// A changed definition every use of which is itself changed in the range (R14),
/// complete in what it searched but for candidates there: the diff already shows its
/// users. A removed one is never among them, since a line that still names it is a
/// broken reference.
fn inside_only(d: &Definition) -> bool {
    d.local_of.is_none()
        && !d.removed()
        && d.rows.is_empty()
        && d.folded.is_empty()
        && d.missed_in.is_empty()
        && d.entry.is_none()
        && d.commits < 2
        && !(d.also_changed.is_empty() && d.possibly_also_changed.is_empty())
        && d.open_only_inside()
}

/// A name no file defines, spelled in files: what the files are, and which of them
/// were only read as text.
fn write_spelled(out: &mut String, a: &Answer, symbol: &str, spelled: &Spelled) {
    let unread = match &spelled.gap {
        Some(Gap::Unaccounted { files, .. }) => format!(
            " in the analysed files; {files} file(s) naming it were read as text, so a definition or caller there is not seen"
        ),
        _ => String::new(),
    };
    // A run that stopped looking has not found there is none.
    let stops = stops_of(&a.doc.coverage.limits);
    let stopped = if stops.is_empty() {
        String::new()
    } else {
        format!(
            " (the run stopped ({}) before it finished looking: a definition may exist)",
            reason_names(&stops)
        )
    };
    let _ = writeln!(
        out,
        "diffctx impact for symbol {symbol}: no definition found{unread}{stopped}; named in {} file(s) ({TEXTUAL_MATCH}):",
        spelled.files
    );
    for row in &spelled.rows {
        let _ = writeln!(out, "  - {}:{}", row.path, row.line);
    }
    let listed = Page::of(&spelled.rows, usize::MAX, 0).total;
    if spelled.files > listed {
        let _ = writeln!(out, "  - and {} more", spelled.files - listed);
    }
    rendered::unanalysed(out, a.doc);
}

/// One line for the definitions only the range's own changes use, each with its
/// users: "nothing outside the diff depends on it" shown, not implied.
fn write_inside_only(out: &mut String, a: &Answer) {
    let inside: Vec<&Definition> = a
        .doc
        .definitions
        .iter()
        .filter(|d| inside_only(d))
        .collect();
    if inside.is_empty() {
        return;
    }
    let page = Page::of(&inside, TOO_COMMON_NAMED, 0);
    let users = |all: &[DefRef], lead: &str| -> Option<String> {
        let page = Page::of(all, MAX_ALSO_CHANGED_SHOWN, 0);
        let shown: Vec<String> = page.shown.iter().map(label).collect();
        let more = match page.rest() {
            0 => String::new(),
            n => format!(" and {n} more"),
        };
        (!shown.is_empty()).then(|| format!("{lead} {}{more}", shown.join(", ")))
    };
    let named: Vec<String> = page
        .shown
        .iter()
        .map(|d| {
            let both: Vec<String> = [
                users(&d.also_changed, "used by"),
                users(&d.possibly_also_changed, "possibly used by"),
            ]
            .into_iter()
            .flatten()
            .collect();
            format!(
                "{} ({})",
                place(&d.path, d.symbol.as_deref(), &d.lines),
                both.join("; ")
            )
        })
        .collect();
    let rest = match page.rest() {
        0 => String::new(),
        n => format!("; and {n} more"),
    };
    let possible = if inside.iter().any(|d| !d.possibly_also_changed.is_empty()) {
        " (a possible user is one whose binding is not resolved)"
    } else {
        ""
    };
    let _ = writeln!(
        out,
        "- changed, used only by definitions changed in this range: {}{rest}{possible}",
        named.join("; ")
    );
}

/// A deleted module nothing imports: one line, never one per definition.
fn write_deleted_modules(out: &mut String, a: &Answer) {
    let modules = &a.doc.deleted_modules;
    for m in &modules.items {
        let found = if m.dynamic_import {
            "no importer found; imports computed at run time are not read"
        } else {
            "no importers"
        };
        let _ = writeln!(
            out,
            "- deleted module {}: {} definition(s), {found}",
            m.path, m.definitions
        );
    }
    if modules.omitted() > 0 {
        let _ = writeln!(
            out,
            "- {} more deleted module(s), no importers",
            modules.omitted()
        );
    }
}

fn write_unfinished(out: &mut String, a: &Answer, stalled: &[&Definition]) {
    let page = Page::of(stalled, TOO_COMMON_NAMED, a.doc.coverage.not_searched);
    let total = page.total;
    if total == 0 {
        return;
    }
    let names: Vec<String> = page
        .shown
        .iter()
        .map(|d| match &d.symbol {
            Some(name) => format!("{}::{name}", d.path),
            None => d.path.clone(),
        })
        .collect();
    let more = page.rest();
    let _ = writeln!(
        out,
        "- callers not determined for {total} changed definition(s): the run stopped ({}) before they were searched{}{}",
        reason_names(&stops_of(&a.doc.coverage.limits)),
        if names.is_empty() {
            String::new()
        } else {
            format!(": {}", names.join(", "))
        },
        if more > 0 && !names.is_empty() {
            format!(", and {more} more")
        } else {
            String::new()
        },
    );
}

/// The definitions the cap shed: one line, the count whole, the budget that
/// shows them, and each with the callers it had.
fn write_omitted(out: &mut String, a: &Answer) {
    let Some(omitted) = &a.doc.omitted else {
        return;
    };
    let count = omitted.definitions.total;
    let named: Vec<String> = omitted
        .definitions
        .items
        .iter()
        .map(|d| {
            let name = match &d.def.symbol {
                Some(symbol) => format!("{}::{symbol}", d.def.path),
                None => d.def.path.clone(),
            };
            format!("{name}:{} ({})", d.def.lines, d.shed.total())
        })
        .collect();
    let callers_omitted = omitted.shed.total();
    // The sentence is whole whatever the list kept; the names follow it, and a
    // trimmed list ends in how many it left out.
    let _ = writeln!(
        out,
        "- {count} changed definition(s) omitted: the answer cap; `--budget {}` shows them{}. Finding no callers here does not mean there are none.{}",
        omitted.needs,
        if callers_omitted > 0 {
            format!(" (their {callers_omitted} caller(s) are not listed)")
        } else {
            String::new()
        },
        if named.is_empty() {
            String::new()
        } else {
            format!(
                " Omitted: {}{}",
                named.join(", "),
                match omitted.definitions.omitted() {
                    0 => String::new(),
                    more => format!(", and {more} more"),
                }
            )
        }
    );
}

fn write_too_common(out: &mut String, symbols: &[&Definition]) {
    let mut languages: Vec<&str> = symbols
        .iter()
        .filter_map(|d| match d.limit() {
            Some(Gap::Ambiguous { language, .. }) => Some(language.as_str()),
            _ => None,
        })
        .collect();
    languages.sort_unstable();
    languages.dedup();
    let _ = writeln!(
        out,
        "- {} changed definition(s) with names too common to trace by name ({} has no binding model here; callers not resolved): {}",
        Page::of(symbols, TOO_COMMON_NAMED, 0).total,
        languages.join(", "),
        named_list(symbols)
    );
}

fn named_list(symbols: &[&Definition]) -> String {
    let page = Page::of(symbols, TOO_COMMON_NAMED, 0);
    let names: Vec<String> = page
        .shown
        .iter()
        .map(|d| format!("{}::{}", d.path, d.symbol.as_deref().unwrap_or("?")))
        .collect();
    let more = page.rest();
    if more > 0 {
        format!("{}, and {more} more", names.join(", "))
    } else {
        names.join(", ")
    }
}

/// The tests that reach a caller, as the entries print them: each `path::name`
/// and `via` where known, the rest counted.
pub(super) fn reachable_from_tests(tests: &[TestLink]) -> String {
    let named = super::named_tests(tests);
    let more = tests.len() - named.len();
    reachable(
        named
            .into_iter()
            .map(|t| (t.path.as_str(), t.name.as_deref(), t.via.as_deref())),
        more,
    )
}

fn reachable<'a>(
    tests: impl Iterator<Item = (&'a str, Option<&'a str>, Option<&'a str>)>,
    more: usize,
) -> String {
    let mut line = String::from("reachable from tests: ");
    for (n, (path, name, via)) in tests.enumerate() {
        if n > 0 {
            line.push_str(", ");
        }
        line.push_str(path);
        if let Some(name) = name {
            let _ = write!(line, "::{name}");
        }
        if let Some(via) = via {
            let _ = write!(line, " via {via}");
        }
    }
    if more > 0 {
        let _ = write!(line, " and {more} more");
    }
    line
}

fn guard_text(row: &Row) -> String {
    let guard = &row.guard;
    let test = guard.test.as_deref().unwrap_or("");
    match guard.state {
        GuardState::Linked => reachable(
            std::iter::once((test, guard.name.as_deref(), guard.via.as_deref())).chain(
                guard
                    .also
                    .iter()
                    .map(|t| (t.test.as_str(), t.name.as_deref(), t.via.as_deref())),
            ),
            guard.more as usize,
        ),
        GuardState::Registered => format!(
            "registered by `@{}`; whatever reads that registry reaches it, which static test links do not follow",
            guard.registrar.as_deref().unwrap_or("")
        ),
        GuardState::EntryPoint => guard.registrar.as_deref().unwrap_or("").to_string(),
        GuardState::Unread => format!(
            "no static test link found; {test} names {} but was not read (over the size cap)",
            row.symbol.as_deref().map_or("the caller", base_symbol)
        ),
        GuardState::FileOnly => "no static test link (its file has tests)".to_string(),
        GuardState::None => "no static test link found".to_string(),
        GuardState::NoTests => "no test files in view".to_string(),
        GuardState::NotEstablished => {
            format!("{NOT_ESTABLISHED}: the search stopped at its hop bound")
        }
    }
}

fn reason_text(reason: Option<&str>) -> &'static str {
    match reason {
        Some("receiver_unresolved") => "receiver unresolved",
        Some("import_unresolved") => "import not resolved",
        Some("ambiguous_module") => "module ambiguous",
        Some("import_glob") => "loaded by an import glob",
        Some("search_path") => "schema left to the search path",
        Some("rebound") => {
            "the name now binds to another definition; this use may have meant the removed one"
        }
        Some("global") => "a global; which scripts load it is not traced",
        _ => "binding unresolved",
    }
}

/// A name that is a local of a definition: where it is declared, whose local
/// it is, and each use inside that definition, with the inner local or
/// definition the use sits in.
fn write_local(out: &mut String, d: &Definition, local: &LocalOf) {
    let _ = writeln!(out, "- {}", place(&d.path, d.symbol.as_deref(), &d.lines));
    let _ = writeln!(
        out,
        "  - `{}` is a local of `{}`, not a definition of its own: its uses are the ones inside it",
        d.symbol.as_deref().unwrap_or(""),
        local.enclosing
    );
    for r in &d.rows {
        let _ = writeln!(
            out,
            "  - used in {}",
            place(&r.path, r.symbol.as_deref(), &r.lines)
        );
    }
    if d.rows.is_empty() {
        let _ = writeln!(out, "  - no use of it was found inside it");
    }
}

/// One changed definition: its line, what limits the search for its callers, and
/// what the search found. `scope` is whether the run's own scope was whole.
fn write_definition(out: &mut String, a: &Answer, d: &Definition, scope: bool) {
    if let Some(local) = &d.local_of {
        return write_local(out, d, local);
    }
    let tests_known = a.doc.tests.known;
    let commits = if d.commits >= 2 {
        format!(" [touched by {} commits]", d.commits)
    } else {
        String::new()
    };
    // A removed definition's range is on the base side, not in the head file.
    let lines = if d.removed() {
        format!("removed; was {}", d.lines)
    } else {
        d.lines.clone()
    };
    let claim = d.guard.as_ref().filter(|_| tests_known).map(|g| {
        if g.state == GuardState::NotEstablished {
            NOT_ESTABLISHED
        } else {
            "no static test link"
        }
    });
    let note = match (&d.entry, claim) {
        (Some(role), Some(claim)) => format!(" \u{2014} {role} \u{2014} {claim}"),
        (None, Some(claim)) => format!(" \u{2014} {claim}"),
        (Some(role), None) => format!(" \u{2014} {role}"),
        (None, None) => String::new(),
    };
    let _ = writeln!(
        out,
        "- {}{commits}{note}",
        place(&d.path, d.symbol.as_deref(), &lines)
    );
    if let Some(gap) = d.limit() {
        // A read nothing types leaves the reads undetermined and the calls known:
        // said apart, so the second does not read as the first.
        if d.coverage.calls.complete
            && !d.coverage.reads.complete
            && !d
                .rows
                .iter()
                .any(|r| matches!(r.class, Class::Call | Class::Implementation))
        {
            let _ = writeln!(out, "  - no call found in the analysed scope");
        }
        // The most limiting gap is said; the others are counted, and the JSON
        // carries them all.
        let others = Page::of(&d.gaps, 1, 0).rest();
        let _ = writeln!(
            out,
            "  - callers not resolved: {}{}",
            gap_sentence(gap),
            if others > 0 {
                format!("; {others} other limit(s)")
            } else {
                String::new()
            }
        );
    }
    if d.removed() && d.rows.is_empty() && d.missed_in.is_empty() && d.also_changed.is_empty() {
        // An absence needs the definition's search complete; said as a fact of
        // the repository only when the run's scope was whole too.
        match (d.complete(), scope) {
            (true, true) => {
                let _ = writeln!(out, "  - nothing references it");
            }
            (true, false) => {
                let _ = writeln!(out, "  - no reference found in the analysed scope");
            }
            (false, _) => {}
        }
    }
    for fold in d.folded.iter().filter(|f| compatible(f)) {
        let _ = writeln!(
            out,
            "  - {} call site(s), signature-compatible: only fields or parameters with defaults were added",
            counted(fold)
        );
    }
    write_resolved_callers(out, d, tests_known);
    write_candidate_callers(out, d, a.ctx.compact);
    write_reexports(out, d);
    write_folded(out, d);
    write_also_changed(out, d);
    if d.shed.total() > 0 {
        let _ = writeln!(
            out,
            "  - {} more caller(s) omitted to fit the cap",
            d.shed.total()
        );
    }
}

/// The calls of a class that only gained fields or parameters with defaults, which
/// the facts have no signature of: the text rule's own fold, with no parameters named.
fn compatible(f: &Fold) -> bool {
    f.rule == "check-the-default" && f.parameters.is_empty() && f.examples.is_empty()
}

/// A fold's count, as the least there are unless its search was whole.
fn counted(f: &Fold) -> String {
    if f.count.exact {
        f.count.n.to_string()
    } else {
        format!("at least {}", f.count.n)
    }
}

/// The rows the change cannot have affected, counted: one line per relation
/// kind and rule, with the way to list them.
fn write_folded(out: &mut String, d: &Definition) {
    for f in d.folded.iter().filter(|f| !compatible(f)) {
        let count = counted(f);
        let examples = f.examples.iter().map(label).collect::<Vec<_>>().join(", ");
        if f.rule == "check-the-default" {
            let _ = writeln!(
                out,
                "  - {count} call(s) compatible with the new optional parameter(s) {}: check that the default keeps the old behaviour — e.g. {examples}; --unfold lists them",
                f.parameters.join(", ")
            );
        } else if f.rule == "untyped-receiver" {
            // A candidate may be affected: said as what it is, never as unaffected.
            let shared = if f.sharing > 0 {
                format!("; up to {} definition(s) share the name", f.sharing)
            } else {
                String::new()
            };
            let _ = writeln!(
                out,
                "  - possible {} through an untyped receiver (not resolved): {count} — e.g. {examples}{shared}; --unfold lists them",
                f.kind
            );
        } else {
            let _ = writeln!(
                out,
                "  - {} not affected ({}): {count} — e.g. {examples}; --unfold lists them",
                f.kind, f.rule
            );
        }
    }
}

/// Callers that are changed in the range too: the diff's own lines, so they
/// are named apart from the callers outside it.
fn write_also_changed(out: &mut String, d: &Definition) {
    if d.also_changed.is_empty() {
        return;
    }
    let page = Page::of(&d.also_changed, MAX_ALSO_CHANGED_SHOWN, 0);
    let shown: Vec<String> = page.shown.iter().map(label).collect();
    let more = page.rest();
    let _ = writeln!(
        out,
        "  - also changed in this diff: {}{}",
        shown.join(", "),
        if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        }
    );
}

fn write_resolved_callers(out: &mut String, d: &Definition, tests_known: bool) {
    // Thirty call sites in one test module are one fact, not thirty
    // lines: the first few name where, the count says how many.
    let mut shown_per_file: FxHashMap<&str, usize> = FxHashMap::default();
    let mut folded_per_file: FxHashMap<&str, (usize, usize)> = FxHashMap::default();
    for r in d.rows.iter().filter(|r| r.is_caller()) {
        let shown = shown_per_file.entry(r.path.as_str()).or_default();
        if *shown >= MAX_CALLERS_SHOWN_PER_FILE {
            let entry = folded_per_file.entry(r.path.as_str()).or_default();
            entry.0 += 1;
            if r.without_link() {
                entry.1 += 1;
            }
            continue;
        }
        *shown += 1;
        let verb = if r.class == Class::Reference {
            "referenced from"
        } else {
            "called from"
        };
        let _ = writeln!(
            out,
            "  - {verb} {} — {}",
            place(&r.path, r.symbol.as_deref(), &r.lines),
            guard_text(r)
        );
    }
    let mut folded: Vec<(&str, (usize, usize))> = folded_per_file.into_iter().collect();
    folded.sort();
    for (path, (more, untested)) in folded {
        let _ = writeln!(
            out,
            "  - and {more} more call site(s) in {path}{}",
            if untested > 0 && tests_known {
                format!(", {untested} without a static test link")
            } else {
                String::new()
            }
        );
    }
}

fn write_reexports(out: &mut String, d: &Definition) {
    for r in d.rows.iter().filter(|r| r.class == Class::Reexports) {
        let _ = writeln!(
            out,
            "  - re-exported from {}",
            place(&r.path, None, &r.lines)
        );
    }
}

fn write_candidate_callers(out: &mut String, d: &Definition, compact: bool) {
    let candidates: Vec<&Row> = d
        .rows
        .iter()
        .filter(|r| r.standing == Standing::Candidate)
        .collect();
    let shown = if compact { 0 } else { MAX_CANDIDATES_SHOWN };
    for r in candidates.iter().take(shown) {
        let _ = writeln!(
            out,
            "  - possibly called from {} ({})",
            place(&r.path, r.symbol.as_deref(), &r.lines),
            reason_text(r.reason.as_deref())
        );
    }
    let page = Page::of(&candidates, shown, 0);
    if page.rest() > 0 {
        let _ = writeln!(
            out,
            "  - and {} {}possible caller(s) whose binding is unresolved",
            page.rest(),
            if shown > 0 { "more " } else { "" }
        );
    }
}
