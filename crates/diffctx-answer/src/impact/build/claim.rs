//! What the answer claims of a changed definition once it is traced: callers it names, tests it
//! finds or lacks, the files that spell its name, and the list the definition lands in.

use crate::answer::v2::GuardState;

use crate::decide::{self};

use diffctx_base::types::Fragment;

use super::super::callers::extension;
use super::super::cells::{REEXPORT, held_has_users};
use super::super::changes::{lines_of, rel};
use super::super::coverage::{absence, search_coverage};
use super::super::dispatch::enclosing_class;
use super::super::output::{Caller, ChangedSymbol, UnanalysedFile, Unknown};
use super::super::unanalysed::{
    MAX_UNTRACED_NAMERS, callable_by_name, left_words, searched_as_text, spelled_name,
    unaccounted_rows,
};

use super::context::{Findings, Impact};
use super::definition::Core;
use super::role::Role;
use super::targets::Targets;

/// Everything traced of one definition before its claims are made.
pub(super) struct Dossier {
    pub(super) callers: Vec<Caller>,
    pub(super) commits: usize,
    pub(super) also_changed: Vec<String>,
    pub(super) possibly_also_changed: Vec<String>,
    pub(super) called_in_diff: bool,
    pub(super) role: Role,
}

pub(super) fn settle(
    cx: &Impact<'_>,
    t: &Targets<'_>,
    c: &Core<'_, '_>,
    dossier: Dossier,
    found: &mut Findings,
) {
    let state = cx.state;
    let query = cx.query;
    let cells = cx.cells;
    let tests = &cx.tests;
    let by_id = &cx.by_id;
    let untraced = &t.untraced;
    let core_place = |core: &Fragment| cx.core_place(core);
    let file_of = |f: &Fragment| cx.file_of(f);
    let link = |user: &Fragment, used: &Fragment| cx.link(user, used);
    let core = c.core;
    let py = c.py;
    let verdict = &c.verdict;
    let member = c.member;
    let compatible = c.compatible;
    let Dossier {
        mut callers,
        commits,
        also_changed,
        possibly_also_changed,
        called_in_diff,
        role,
    } = dossier;
    let Role {
        route,
        entry,
        linkless_role,
        unresolved,
    } = role;
    // A method only dispatch reaches is reached through its class: a test
    // handing the class to the runtime (a server, a test runner, a
    // visitor) reaches the method (#578). A caller a test calls is the
    // second hop of a test link: a caller read off the syntax has no
    // graph edge for the walk to take (#503). A candidate caller counts: a
    // test that reaches it may reach the definition, and an absence is
    // claimed only where nothing could.
    let class = route
        .as_ref()
        .and_then(|_| enclosing_class(core, file_of(core)));
    let class_tested =
        || class.is_some_and(|class| !tests.tested_by(class, state, by_id, &link).is_empty());
    let tests_absent = query.is_none()
        && tests.any()
        && !tests.is_test(core)
        && (entry.is_none() || linkless_role)
        && !callers
            .iter()
            .any(|c| c.tests.iter().any(|t| t.via.is_none()))
        && tests.tested_by(core, state, by_id, &link).is_empty()
        && !class_tested();
    let compatible_callers = if compatible {
        let n = callers
            .iter()
            .filter(|c| !matches!(c.evidence, "candidate" | REEXPORT))
            .count();
        callers.clear();
        n
    } else {
        0
    };
    let mut missed_in: Vec<UnanalysedFile> = verdict
        .as_ref()
        .map_or_else(Vec::new, |v| unaccounted_rows(v, &rel(state, core.path())));
    let mut outside = 0;
    let nothing_known = callers.is_empty()
        && compatible_callers == 0
        && (unresolved.is_none()
            || verdict
                .as_ref()
                .is_some_and(decide::verdict::Verdict::gaps_are_only_unaccounted))
        && missed_in.is_empty();
    // A script calls a shell or Ruby definition by its bare name: a CI step's
    // `run:` block, a Rakefile, a script with no extension.
    let runs_script = |file: &UnanalysedFile| {
        let name = file.path.rsplit('/').next().unwrap_or(&file.path);
        matches!(
            diffctx_base::languages::get_language_for_file(core.path()),
            Some("bash" | "ruby")
        ) && (!name.contains('.') || matches!(extension(name).as_str(), "yml" | "yaml" | "rake"))
    };
    let left = || py.into_iter().flat_map(left_words);
    let spelled = core
        .symbol_name
        .iter()
        .map(String::as_str)
        .chain(left())
        .flat_map(|n| untraced.literal_files(n, member))
        .map(|file| (file, true));
    // The linker exports a non-static C function, and cgo a `//export`ed Go one:
    // another language that loads the library names it by its bare name.
    let linked = py.is_some_and(|def| {
        cells
            .of(&def.file)
            .is_some_and(|cell| cell.linked_by_name_at(&def.file, &def.name, def.lines))
    });
    let named = untraced
        .of(state, core, left(), linked)
        .into_iter()
        .filter(|f| nothing_known || runs_script(f))
        .map(|file| (file, false))
        .chain(spelled);
    // A config that spells the name is no test: only a test file that does
    // leaves the absence of a test link open.
    let mut outside_tests = 0;
    let mut listed_namers = 0;
    let mut unlisted_namers = 0;
    // A string on the line of a call that computes a name (`globals()["handle_" +
    // kind]`) is that call's: the route no model traces, already named.
    let computes = |file: &UnanalysedFile| {
        verdict
            .as_ref()
            .is_some_and(decide::verdict::Verdict::not_traced)
            && cells.of(&file.path).is_some_and(|cell| {
                file.lines
                    .iter()
                    .any(|line| cell.computes_name_at(&file.path, *line))
            })
    };
    for (file, literal) in named {
        if !missed_in.iter().any(|m| m.path == file.path)
            && !(literal
                && (computes(&file)
                    || verdict.as_ref().is_some_and(|v| {
                        file.lines
                            .iter()
                            .any(|line| v.names_another(&file.path, *line))
                    })))
            && !callers.iter().any(|c| c.path == file.path)
            && !(literal
                && callers
                    .iter()
                    .any(|c| c.tests.iter().any(|t| t.path == file.path)))
            && !verdict
                .as_ref()
                .is_some_and(|v| v.uses.iter().any(|u| u.file == file.path))
            && !matches!(&unresolved, Some(Unknown::Candidates { sites, .. })
                    if sites.iter().any(|s| s.path == file.path))
        {
            outside += 1;
            outside_tests += usize::from(!literal || tests.is_test_file(&file.path));
            if listed_namers < MAX_UNTRACED_NAMERS {
                listed_namers += 1;
                missed_in.push(file);
            } else {
                unlisted_namers += 1;
            }
        }
    }
    let tests_coverage = search_coverage(verdict.as_ref(), unresolved.as_ref(), outside_tests);
    // An absence claimed for a name never searched as text is no witness.
    let unsearched =
        nothing_known
            && outside == 0
            && also_changed.is_empty()
            && possibly_also_changed.is_empty()
            && entry.is_none()
            && callable_by_name(core.path())
            && spelled_name(core).is_some_and(|n| !searched_as_text(n))
            && (verdict
                .as_ref()
                .is_some_and(decide::verdict::Verdict::names_quoted)
                || untraced.unread.could_spell(
                    diffctx_resolve::vocabulary::family::Group::of_name(&rel(state, core.path())),
                ));
    let coverage = search_coverage(verdict.as_ref(), unresolved.as_ref(), outside)
        .with(unsearched.then_some(decide::gap::Gap::NameUnsearched));
    // No test found is an absence only from a closed search: the callers
    // accounted for and every definition above them. An open search says the
    // tests are not established; incomplete callers say nothing here, their
    // own gap does.
    let untested = tests_absent
        .then(|| {
            if tests.open_above(core, &link, false)
                || class.is_some_and(|class| tests.open_above(class, &link, false))
            {
                Some(GuardState::NotEstablished)
            } else {
                tests_coverage.all().witness().map(|_| GuardState::None)
            }
        })
        .flatten();
    let absent = untested == Some(GuardState::None);
    found.untested_definitions += usize::from(absent);
    // A test that reaches a definition no caller does (a doctest runs it, a
    // test in its own file spells it, a test of the class a dispatch route
    // goes through) is no caller: the definition is listed, its guard stated
    // by the absence of the claim, not left to the lists of what is untested.
    let tested_without_caller = || {
        query.is_none()
            && tests.any()
            && !tests.is_test(core)
            && entry.is_none()
            && untested.is_none()
            && (class_tested() || !tests.tested_by(core, state, by_id, &link).is_empty())
    };
    // Nothing said of a definition needs every relation of it known; a test link
    // nobody found and nobody ruled out is not known, and the lists here claim none.
    if let Some(complete) = coverage.all().witness().filter(|_| {
        callers.is_empty()
            && compatible_callers == 0
            && commits < 2
            && query.is_none()
            && also_changed.is_empty()
            && possibly_also_changed.is_empty()
            && !tested_without_caller()
            && (absent || !tests_absent)
    }) {
        if entry.is_none()
            && py.is_some_and(|def| {
                cells
                    .of(&def.file)
                    .is_some_and(|cell| held_has_users(cell, def))
            })
        {
            found.traced_definitions -= 1;
            return;
        }
        // Counted in the header, so named: a count of nothing listed
        // tells the reader neither where nor what (#398).
        let named = core_place(core);
        if absent {
            let named = match entry.as_ref().filter(|_| linkless_role) {
                Some(role) => format!("{named} \u{2014} {role}"),
                None => named,
            };
            if called_in_diff {
                found.untested_called_in_diff.push(named);
            } else {
                found.untested_unlisted.push(absence(named, complete));
            }
        } else if let Some(role) = entry.as_ref() {
            found
                .entry_unlisted
                .push(format!("{named} \u{2014} {role}"));
        } else if called_in_diff {
            found.untested_called_in_diff.push(named);
        } else {
            // No test is in view to claim a missing link against: the search
            // still found no caller, and the definition is named for that.
            found.untested_unlisted.push(absence(named, complete));
        }
        return;
    }
    // Listed for the callers changed with it, which are the diff's own
    // lines: the definition is live, and a missing test link is said once,
    // in the list a definition nothing outside the diff calls goes in.
    let only_in_diff = callers.is_empty()
        && compatible_callers == 0
        && commits < 2
        && coverage.all().witness().is_some()
        && !(also_changed.is_empty() && possibly_also_changed.is_empty());
    if only_in_diff && absent {
        found.untested_called_in_diff.push(core_place(core));
    }
    found.changed.push(ChangedSymbol {
        path: rel(state, core.path()),
        symbol: core.symbol_name.clone(),
        lines: lines_of(&core.id),
        kind: format!("{:?}", core.kind).to_lowercase(),
        commits,
        callers,
        entry,
        unresolved,
        untested: untested.filter(|_| !only_in_diff),
        compatible_callers,
        missed_in,
        also_changed,
        possibly_also_changed,
        shed: crate::answer::v2::ShedRows {
            name: unlisted_namers,
            ..Default::default()
        },
        change: py.map(crate::answer::v2::Change::of),
        callers_omitted: 0,
        omitted: Vec::new(),
        folded: Vec::new(),
        new_parameters: py.map_or_else(Vec::new, decide::changed::Changed::new_parameters),
        coverage,
    });
}
