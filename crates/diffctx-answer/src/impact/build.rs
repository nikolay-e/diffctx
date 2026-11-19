//! The impact answer of a changed range: every changed definition with its callers, tests, removals and
//! unknowns.

use std::path::Path;

use std::time::Instant;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::answer::render::place;

use crate::answer::v2::GuardState;

use crate::decide::cells::Cells;
use crate::decide::{self};

use crate::run_provenance::state::ScoredState;

use diffctx_base::container::{innermost, named_container_of, within};

use diffctx_base::types::{Fragment, FragmentId, FragmentKind};

use diffctx_facts::facts::{EntryKind, framework};

use diffctx_facts::parsers::code_text;

use diffctx_occurrence::tokens::occurrence::Standing;

use diffctx_resolve::vocabulary;

use super::mentions::{named_by_path, stale_references, twin_references};

use super::tests_reach::TestsReach;

use super::IMPACT_TOKEN_CAP;
use super::MAX_BLAMED_SYMBOLS;
use super::MAX_CONTRACTS;
use super::MAX_DEFINITIONS;
use super::callers::{
    Files, Receiver, binds_locally, extension, first_naming_line, is_code_file, is_import_block,
    is_member, mentions, module_aliases, references_module,
};
use super::cells::{
    FINDING_WEIGHT, KEY_MATCH, REEXPORT, Reach, cell_anchor, cell_caller, cell_row, cell_symbol,
    cell_unknown, held_has_users, is_module_block_binding, lifted_caller,
    owned_by_a_nested_definition, reach_of,
};
use super::changes::{
    BaseTexts, changed_rels, commit_bounds, commits_touching, head_of, is_public, is_schema_file,
    layout_files, lines_of, moved_verbatim, names_of, rel, same_runtime, signature_changed,
    spelling_files, tool_contract_changed, unread_namers, without_decorators,
};
use super::coverage::{absence, search_coverage, uncovered_symbol, unfinished_symbol};
use super::deliver::{proves_empty, relation_rank};
use super::dispatch::{base_symbol, dispatch_route, enclosing_class, indent_of, merge_by_symbol};
use super::entry::{
    Entered, compatible_extension, entered_by, http_handler_base, is_jvm_path, is_program_main,
    is_pytest_fixture, jvm_entry_point, registering_decorator,
};
use super::key_search::{key_references, read_by_collection};
use super::output::{Caller, ChangedSymbol, Contract, ImpactOutput, UnanalysedFile, Unknown};
use super::removed::{dead_modules, removed_definitions, removed_route};
use super::symbol::{directory_name, parse_symbol_query};
use super::unanalysed::{
    MAX_OUTSIDE_RUNS, MAX_UNTRACED_NAMERS, OUTSIDE_DEFINITIONS, UntracedNamers, callable_by_name,
    later_needles, left_words, literal_base, nameless_changes, outside_definitions,
    searched_as_text, spelled_name, spelled_word, unaccounted_rows, unanalysed_files,
};

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
    // A signature variant as long as its fragment shares its id (a Kotlin
    // class header ending `) {`); the id names the fragment, not its stub.
    let mut by_id: FxHashMap<&FragmentId, &Fragment> = FxHashMap::default();
    for f in &state.all_fragments {
        by_id
            .entry(&f.id)
            .and_modify(|seen| {
                if seen.kind.is_signature() && !f.kind.is_signature() {
                    *seen = f;
                }
            })
            .or_insert(f);
    }
    let changed_paths: FxHashSet<&str> = state
        .changed_files
        .iter()
        .filter_map(|p| p.to_str())
        .collect();
    // The cells: changed definitions from the facts layer's semantic changes,
    // callers from the resolver of their language (decide.rs). The ranking
    // graph and the text filters take no part in what they answer.
    let analysed = state.analysed_range.as_deref().or(diff_range);
    let cells = {
        let _t = diffctx_base::profile::detail("impact.setup.cells", "");
        Cells::open(state, analysed, query.is_some())
    };
    // A hunk inside a function body seeds a nested fragment — a `let`, an
    // inner block — that nobody calls; the symbol a reader knows, and the one
    // the callers link to, is the named container around it.
    let mut cores: Vec<&Fragment> = state
        .core_ids
        .iter()
        .filter_map(|id| by_id.get(id).copied())
        .filter_map(|f| named_container_of(f, &state.all_fragments))
        .collect();
    cores.sort_by(|a, b| a.id.cmp(&b.id));
    cores.dedup_by(|a, b| a.id == b.id);
    let later = later_needles(state, &cores);
    let tests = {
        let _t = diffctx_base::profile::detail("impact.setup.tests", "");
        TestsReach::build(state, &cells, analysed, &later)
    };
    // A caller in a changed file is outside the diff unless its lines are
    // in a hunk: the reader holds the hunks, not the rest of the file (#362).
    let mut hunk_lines: FxHashMap<&str, Vec<(u32, u32)>> = FxHashMap::default();
    for h in &state.hunk_texts {
        // A pure removal sits between new lines N and N+1: as `(N+1, N)` the
        // overlap test below holds only for a fragment spanning both, not
        // for the caller that ends on N right above the deleted lines.
        let range = if h.new_len == 0 {
            (h.new_start + 1, h.new_start)
        } else {
            h.new_range()
        };
        hunk_lines.entry(h.path.as_str()).or_default().push(range);
    }
    let in_a_hunk = |id: &FragmentId| {
        if state.hunk_texts.is_empty() {
            return changed_paths.contains(id.path.as_ref());
        }
        hunk_lines
            .get(rel(state, id.path.as_ref()).as_str())
            .is_some_and(|ranges| {
                ranges
                    .iter()
                    .any(|&(a, b)| id.start_line <= b && id.end_line >= a)
            })
    };
    let hunks_in = |core: &Fragment| -> Vec<&diffctx_base::changed_lines::HunkText> {
        let path = rel(state, core.path());
        state
            .hunk_texts
            .iter()
            .filter(|h| {
                let (a, b) = h.new_range();
                h.path == path && core.start_line() <= b && core.end_line() >= a
            })
            .collect()
    };
    let core_place = |core: &Fragment| {
        place(
            &rel(state, core.path()),
            core.symbol_name.as_deref(),
            &lines_of(&core.id),
        )
    };
    let mut untested_definitions = 0usize;
    let mut untested_unlisted: Vec<String> = Vec::new();
    let mut untested_called_in_diff: Vec<String> = Vec::new();
    let mut entry_unlisted: Vec<String> = Vec::new();
    let mut layout_only: Vec<String> = Vec::new();
    let mut traced_definitions = 0usize;
    let mut traced_names: Vec<String> = Vec::new();

    let mut held: FxHashMap<String, Vec<(u32, u32)>> = FxHashMap::default();
    for c in &cores {
        held.entry(rel(state, c.path()))
            .or_default()
            .push((c.start_line(), c.end_line()));
    }

    let bounds = commit_bounds(analysed).filter(|_| state.commit_count >= 2);
    let base_texts = BaseTexts::new(&state.root_dir, state.run.source().base());

    let mut changed: Vec<ChangedSymbol> = Vec::new();
    // Schema changes first, then the public symbols something calls, then
    // the rest: what the cap keeps is what a reader would look at first.
    let mut contracts: Vec<Contract> = Vec::new();
    let mut public_api: Vec<(bool, Contract)> = Vec::new();
    // Definitions are counted per language: the builders link a call to the
    // definitions of its own language, so a Python wrapper named like a Rust
    // function is not what makes the Rust name ambiguous. An `impl` block
    // extends the type it names rather than competing with it: a struct with
    // a `Display` and an `Error` impl is one definition, not three.
    let mut definitions: FxHashMap<(&str, String), usize> = FxHashMap::default();
    let mut defined_in: FxHashSet<(String, &str)> = FxHashSet::default();
    let mut file_fragments: FxHashMap<&str, Vec<&Fragment>> = FxHashMap::default();
    let indexing = diffctx_base::profile::detail("impact.setup.files", "");
    for f in &state.all_fragments {
        file_fragments.entry(f.path()).or_default().push(f);
        if let Some(name) = f
            .symbol_name
            .as_deref()
            .filter(|_| f.kind.is_definition_kind() && f.kind != FragmentKind::Impl)
        {
            let lang = diffctx_base::languages::get_language_for_file(f.path()).unwrap_or("");
            *definitions.entry((lang, name.to_lowercase())).or_default() += 1;
            defined_in.insert((name.to_string(), f.path()));
        }
    }
    drop(indexing);
    let files = Files {
        fragments: file_fragments,
        cells: &cells,
        state,
    };
    let source = state.run.source();
    // A lexical link stops at a project the workspace model says cannot
    // reference the other: a Python test naming `config` is not a test of
    // another project's `config.ts`. It crosses through a declared
    // dependency — a Maven `it/` module importing `com.acme.core.Pricing`, a
    // workspace crate writing `core_crate::pricing`.
    let one_tree =
        |user: &str, used: &str| source.may_reference(&rel(state, user), &rel(state, used));
    let one_project = |user: &Fragment, used: &Fragment| one_tree(user.path(), used.path());
    let file_of = |f: &Fragment| files.fragments.get(f.path()).map_or(&[][..], Vec::as_slice);
    let metric_readers = std::cell::LazyCell::new(|| {
        diffctx_graph::keytable::metric_readers(
            &state.all_fragments,
            Some(state.root_dir.as_path()),
            state.run.changed_lines(),
        )
    });
    let registration = |f: &Fragment| -> Option<String> {
        let path = rel(state, f.path());
        let stated_entry = f.symbol_name.as_deref().map(base_symbol).and_then(|name| {
            cells
                .of(&path)?
                .framework_entries_at(&path, name, (f.start_line(), f.end_line()))
                .into_iter()
                .find(|(kind, _)| *kind != EntryKind::Fixture)
        });
        if let Some((kind, entry)) = stated_entry {
            return Some(framework::describe(kind, &entry));
        }
        if is_jvm_path(f.path()) {
            // Kotlin's and Java's annotations are the facts' own.
            let stated = cells.of(&path).and_then(|cell| {
                cell.annotations_at(
                    &path,
                    f.symbol_name.as_deref().map(base_symbol)?,
                    (f.start_line(), f.end_line()),
                )
            });
            jvm_entry_point(f, &cells.imports_at(&path), stated.as_deref()).or_else(|| {
                let name = f.symbol_name.as_deref().map(base_symbol)?;
                cells
                    .of(&path)?
                    .serialized_at(&path, name, (f.start_line(), f.end_line()))
                    .then(|| format!("{}serialized by Jackson", framework::ENTRY_PREFIX))
            })
        } else {
            registering_decorator(f, file_of(f))
        }
    };
    let is_tool = |f: &Fragment| -> bool {
        let path = rel(state, f.path());
        f.symbol_name
            .as_deref()
            .map(base_symbol)
            .is_some_and(|name| {
                cells.of(&path).is_some_and(|cell| {
                    cell.framework_entries_at(&path, name, (f.start_line(), f.end_line()))
                        .into_iter()
                        .any(|(kind, _)| kind == EntryKind::Tool)
                })
            })
    };
    let link = |user: &Fragment, used: &Fragment| -> bool {
        let Some(name) = used.symbol_name.as_deref().map(base_symbol) else {
            return false;
        };
        if !same_runtime(user.path(), used.path()) {
            return false;
        }
        let (user_rel, used_rel) = (rel(state, user.path()), rel(state, used.path()));
        if let Some(cell) = cells.of(&user_rel).filter(|c| c.owns(&used_rel)) {
            return cell.links(
                &user_rel,
                (user.start_line(), user.end_line()),
                &used_rel,
                name,
                (used.start_line(), used.end_line()),
            );
        }
        one_project(user, used)
            && mentions(user, Some(name), &Receiver::Any)
            && references_module(user, &files, Some(name), used.path())
    };
    let reaches_file = |test: &Fragment, caller: &Fragment| -> bool {
        if !same_runtime(test.path(), caller.path()) {
            return false;
        }
        let (test_rel, caller_rel) = (rel(state, test.path()), rel(state, caller.path()));
        if let Some(cell) = cells.of(&test_rel).filter(|c| c.owns(&caller_rel)) {
            return cell.reaches(&test_rel, &caller_rel);
        }
        one_project(test, caller) && references_module(test, &files, None, caller.path())
    };
    // What the tests say of a caller: the ones that reach it, and why an absent
    // link is or is not a finding.
    // A caller of a changed fragment (`core`) is open too where a test asserts
    // that fragment's text.
    let test_links = |frag: &Fragment, core: Option<&Fragment>| -> Caller {
        let tested = tests.tested_by(frag, state, &by_id, &link);
        let registered_by = tested.is_empty().then(|| registration(frag)).flatten();
        Caller {
            file_tested: tested.is_empty()
                && registered_by.is_none()
                && tests.file_tested(frag, &reaches_file),
            unread_test: tested
                .is_empty()
                .then(|| tests.unread_naming(frag, &rel(state, frag.path())))
                .flatten(),
            tests_open: tested.is_empty()
                && core.map_or_else(
                    || tests.open_above(frag, &link, true),
                    |core| tests.caller_open(frag, core, &link),
                ),
            tests_partial: !tested.is_empty() && tests.nearest_only(frag, state, &by_id, &link),
            registered_by,
            tests: tested,
            ..Caller::default()
        }
    };
    // A field changes its class too: how the class is built (a dataclass's
    // constructor, a column's default) is its callers' business, and what
    // reads the field is the field's (#388, #488); so does an attribute the
    // method around it sets (#426). A query names one definition.
    let holding: Vec<&Fragment> = cores
        .iter()
        .filter(|f| query.is_none() && f.kind == FragmentKind::Property)
        .filter_map(|f| {
            file_of(f)
                .iter()
                .copied()
                .filter(|c| {
                    c.id != f.id
                        && (c.kind.is_container() || c.kind == FragmentKind::Function)
                        && c.symbol_name.is_some()
                        && within(&f.id, c)
                })
                .min_by_key(|c| c.line_count())
        })
        .collect();
    let mut cores = innermost(cores);
    cores.extend(holding);
    // An `impl` block extends the type it names: with that type among the
    // cores, a second entry for the block repeats the type (#403).
    let types: FxHashSet<(&str, &str)> = cores
        .iter()
        .filter(|c| c.kind != FragmentKind::Impl)
        .filter_map(|c| Some((c.path(), c.symbol_name.as_deref()?)))
        .collect();
    let cores: Vec<&Fragment> = cores
        .into_iter()
        .filter(|c| {
            c.kind != FragmentKind::Impl
                || !c
                    .symbol_name
                    .as_deref()
                    .is_some_and(|s| types.contains(&(c.path(), s)))
        })
        .collect();
    // A deletion that only borders a definition removed whatever followed
    // it, at its own indentation or shallower: the definitions it took are
    // reported as removed, this one did not change — and it is no part of
    // the diff when it calls another changed symbol. Lines removed between
    // two lines the definition spans are its own body, at any indentation
    // (a dedented SQL string). A core no hunk overlaps was attached to a
    // deletion beside it: the hunks name its file, never its lines.
    let borders_only = |core: &Fragment| -> bool {
        let core_hunks = hunks_in(core);
        if core_hunks.is_empty() {
            let core_path = rel(state, core.path());
            return state.hunk_texts.iter().any(|h| h.path == core_path);
        }
        let header_indent = indent_of(without_decorators(&core.content));
        core_hunks.iter().all(|h| {
            let interior = core.start_line() <= h.new_start && core.end_line() > h.new_start;
            h.added.is_empty()
                && !interior
                && h.removed
                    .iter()
                    .find(|l| !l.trim().is_empty())
                    .is_some_and(|l| indent_of(l) <= header_indent)
        })
    };
    let bordering: FxHashSet<&FragmentId> = cores
        .iter()
        .filter(|c| borders_only(c))
        .map(|c| &c.id)
        .collect();
    // A constructor is called by its class's name (`cp.ComponentModel(b, 1)`,
    // `new Store()`): its callers are the class's (#318). Mapped after the
    // bordering test, which a class header with no hunk of its own would fail.
    // The span of the constructor a class stands for, so that a queried `A.__init__` is
    // defined by the constructor (and the `super()` calls of subclasses reach it), not by
    // the class header alone.
    let mut constructor_spans: FxHashMap<&FragmentId, (u32, u32)> = FxHashMap::default();
    let cores: Vec<&Fragment> = {
        let mut seen: FxHashSet<&FragmentId> = FxHashSet::default();
        cores
            .into_iter()
            .map(|c| {
                let constructor = c
                    .symbol_name
                    .as_deref()
                    .map(base_symbol)
                    .is_some_and(|s| matches!(s, "__init__" | "constructor"));
                let class = constructor
                    .then(|| enclosing_class(c, file_of(c)))
                    .flatten();
                if let Some(class) = class {
                    constructor_spans.insert(&class.id, (c.start_line(), c.end_line()));
                }
                class.unwrap_or(c)
            })
            .filter(|c| seen.insert(&c.id))
            .collect()
    };
    // The definitions of the cells' languages are the facts layer's: one entry
    // per changed definition, anchored on the fragment that spans it, so the
    // roles, test links and contracts built on fragments still reach it.
    let mut cores = cores;
    let mut cell_defs: FxHashMap<&FragmentId, decide::changed::Changed> = FxHashMap::default();
    let mut cell_apart: Vec<decide::changed::Changed> = Vec::new();
    // A file over the size cap is named as such; its definitions, one per
    // unanchored entry, are not each traced apart (gh#619).
    let over_the_cap = state
        .run
        .limited_by(diffctx_snapshot::resource::LimitReason::FileTooLarge);
    let mut cell_removed: Vec<decide::changed::Changed> = Vec::new();
    let mut cell_untracked: Vec<&Fragment> = Vec::new();
    // A queried name in a file its cell lost (a syntax error, a read): the
    // definition is not in the facts, and the file's loss is the answer.
    let mut cell_unread: Vec<(&Fragment, decide::gap::Reason)> = Vec::new();
    let asked = query
        .and_then(parse_symbol_query)
        .and_then(|(_, segments)| segments.last().copied());
    for cell in cells.iter() {
        let mut entries: Vec<decide::changed::Changed> = Vec::new();
        if query.is_some() {
            for core in cores.iter().filter(|c| cell.owns(&rel(state, c.path()))) {
                let (file, lines) = (
                    rel(state, core.path()),
                    (core.start_line(), core.end_line()),
                );
                let mut defined = cell.define(
                    &file,
                    core.symbol_name.as_deref().map_or("", base_symbol),
                    lines,
                );
                // A second declarator on the fragment's lines is the queried
                // name, which the fragment is not named for.
                let own = asked.map_or_else(Vec::new, |name| {
                    let span = constructor_spans.get(&core.id).copied().unwrap_or(lines);
                    cell.define(&file, name, span)
                });
                if !own.is_empty() {
                    defined = own;
                }
                // A name asked for that no definition stands for is said, not
                // left out: an empty answer would read as "no callers".
                if defined.is_empty() && is_module_block_binding(core, file_of(core)) {
                    cell_untracked.push(core);
                } else if defined.is_empty() {
                    if let Some(reason) = cell.loss_about(&rel(state, core.path())) {
                        let core: &Fragment = core;
                        cell_unread.push((core, reason));
                    }
                }
                entries.extend(defined);
            }
        } else {
            entries.extend(cell.changed().iter().cloned());
        }
        let (owned, rest): (Vec<&Fragment>, Vec<&Fragment>) = std::mem::take(&mut cores)
            .into_iter()
            .partition(|c| cell.owns(&rel(state, c.path())));
        cores = rest;
        let mut seen_entries: FxHashSet<(String, String, (u32, u32))> = FxHashSet::default();
        for entry in entries {
            if !seen_entries.insert((entry.file.clone(), entry.name.clone(), entry.lines)) {
                continue;
            }
            if entry.how == decide::changed::How::Removed {
                cell_removed.push(entry);
                continue;
            }
            let anchor = files
                .fragments
                .get(diffctx_base::paths::join_native(&state.root_dir, &entry.file).as_str())
                .and_then(|frags| cell_anchor(frags, &entry));
            match anchor {
                Some(f) if !cell_defs.contains_key(&f.id) => {
                    cores.push(f);
                    cell_defs.insert(&f.id, entry);
                }
                None if over_the_cap.contains(&entry.file) => {}
                _ => cell_apart.push(entry),
            }
        }
        // A definition the facts layer reports no change in, whose hunks are
        // only what a formatter makes (a trailing space, a comma), is layout.
        if query.is_none() {
            for core in owned {
                if cell_defs.contains_key(&core.id) {
                    continue;
                }
                let hunks = hunks_in(core);
                if !hunks.is_empty()
                    && hunks
                        .iter()
                        .all(|h| h.layout || moved_verbatim(h, &state.hunk_texts))
                {
                    let named = core_place(core);
                    if !layout_only.contains(&named) {
                        layout_only.push(named);
                    }
                }
            }
        }
    }
    // A family the run could not resolve: its definitions are named as not
    // determined, never answered by the paths the cells replaced.
    let (cell_lost, kept): (Vec<&Fragment>, Vec<&Fragment>) = std::mem::take(&mut cores)
        .into_iter()
        .partition(|c| cells.lost(&rel(state, c.path())));
    cores = kept;
    // A queried name has no diff: a call from elsewhere in its own file is a
    // caller like any other.
    let inside_the_diff = |id: &FragmentId| {
        (state.core_ids.contains(id) && !bordering.contains(id))
            || (query.is_none() && in_a_hunk(id))
    };
    let untraced = {
        let _t = diffctx_base::profile::detail("impact.setup.untraced", "");
        let removed: Vec<&str> = cell_removed
            .iter()
            .chain(&cell_apart)
            .filter_map(spelled_word)
            .collect();
        let own: Vec<super::keys::OwnSpan> = cell_defs
            .values()
            .chain(&cell_apart)
            .filter_map(|e| spelled_word(e).map(|name| (e.file.clone(), name.to_string(), e.lines)))
            .chain(
                cores
                    .iter()
                    .filter(|c| !cell_defs.contains_key(&c.id))
                    .map(|c| {
                        let name = c.symbol_name.as_deref().map_or("", literal_base);
                        (
                            rel(state, c.path()),
                            name.to_string(),
                            (c.start_line(), c.end_line()),
                        )
                    }),
            )
            .collect();
        let left: Vec<&str> = cell_defs
            .values()
            .chain(&cell_apart)
            .flat_map(left_words)
            .collect();
        UntracedNamers::build(state, &cells, &cores, &removed, &left, &own, diff_range)
    };
    drop(setup);
    diffctx_base::profile::count("impact.cores", cores.len() as u64);
    let stopped_early = !state.run.stops().is_empty();
    let mut unreached: Vec<&Fragment> = cell_lost;
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
        let core_hunks = hunks_in(core);
        let py = cell_defs.get(&core.id);
        let verdict = py.and_then(|def| {
            let cell = cells.of(&def.file)?;
            let _t = diffctx_base::profile::detail("impact.callers.verdict", "");
            Some(cell.verdict(def))
        });
        if let Some(v) = &verdict {
            tracing::debug!(
                "impact {}: the cell claims {:?}",
                core.symbol_name.as_deref().unwrap_or("?"),
                v.claim(diffctx_resolve::resolve::Relation::Calls)
            );
        }
        // A formatter run or a comment changes nothing a caller sees (#370),
        // and neither does a definition moved verbatim within its file. The
        // facts layer says it for its languages: the hashes of the definition
        // did not move.
        // The run's own formatter test (`settle_layout`) still decides what the
        // hashes cannot see through: a trailing comma, a quote style.
        let hunk_layout = !core_hunks.is_empty()
            && core_hunks
                .iter()
                .all(|h| h.layout || moved_verbatim(h, &state.hunk_texts));
        // A decorated definition (a decorator may read what the edit touched) or a
        // type whose data members swapped places is never set aside as layout. A
        // comment-only edit is traced and folds as no code change; a doc edit is listed.
        let layout = match py {
            Some(def) => {
                (matches!(def.how, decide::changed::How::Edited { layout_only: true })
                    || hunk_layout)
                    && !def.not_layout
            }
            None => hunk_layout,
        };
        if query.is_none() && layout {
            layout_only.push(core_place(core));
            continue;
        }
        if query.is_none() && py.is_none() && bordering.contains(&core.id) {
            continue;
        }
        traced_definitions += 1;
        traced_names.extend(core.symbol_name.as_deref().and_then(|s| names_of(s).pop()));
        // A function whose signature the cell found compatible is folded by its
        // rule (`check-the-default`); the text rule below stays for what the cell
        // has no signature of: the fields a class gained.
        let compatible = query.is_none()
            && py.is_none_or(|def| def.new_parameters().is_empty())
            && compatible_extension(core, &core_hunks);
        let symbol = core.symbol_name.as_deref();
        let member = is_member(core, file_of(core));
        let stem = Path::new(core.path())
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        // `build`, `new`, `run`: a name defined all over the repository links
        // by name to every definition, and its "callers" would be everyone's.
        let core_lang = diffctx_base::languages::get_language_for_file(core.path()).unwrap_or("");
        // With a cell, imports decide which definition a reference reaches; a
        // name defined in many files is ambiguous only without one.
        let binding_model = symbol.is_some() && py.is_some();
        // A qualified query (`path:name`) names its definition; only a bare
        // name or a diff leaves the lexical model guessing among many.
        let qualified = query.is_some_and(|q| q.contains(':'));
        let ambiguous = !binding_model
            && !qualified
            && symbol
                .map(|s| {
                    definitions
                        .get(&(core_lang, s.to_lowercase()))
                        .copied()
                        .unwrap_or(0)
                        > MAX_DEFINITIONS
                })
                .unwrap_or(false);
        let mut sources: Vec<(&Fragment, f64, Option<Reach>)> = Vec::new();
        // Why a linked fragment is not listed, per core, under RUST_LOG=debug:
        // the precision filters are the part of this view that field reports
        // question first.
        let mut dropped: FxHashMap<&'static str, usize> = FxHashMap::default();
        // Sources the diff holds go through the same tests as any caller: one
        // that passes is no caller outside the diff, but it makes the
        // definition live rather than one nothing calls (#441).
        let mut in_diff: FxHashSet<FragmentId> = FxHashSet::default();
        // Whether a fragment that reaches the core is a caller, and with
        // what evidence; the reason it is not, otherwise.
        let judge = |frag: &Fragment| -> Result<Option<Reach>, &'static str> {
            // A file that defines the same name calls its own.
            let shadowed = frag.path() != core.path()
                && symbol.is_some_and(|s| defined_in.contains(&(s.to_string(), frag.path())));
            if frag.kind == FragmentKind::Excerpt {
                Err("excerpt")
            } else if !same_runtime(frag.path(), core.path()) {
                Err("other_runtime")
            } else if shadowed && !binding_model {
                Err("defines_same_name")
            } else if is_import_block(&frag.content) {
                Err("import_block")
            } else if !binding_model
                && symbol.is_some_and(|s| {
                    binds_locally(&code_text(frag.path(), &frag.content), base_symbol(s))
                })
            {
                // `let read = |p| p + 1; read(1)` calls its own closure (#391).
                Err("local_binding")
            } else if !is_code_file(frag.path()) {
                Err("not_code")
            } else if !one_tree(frag.path(), core.path()) {
                Err("other_project")
            } else if !{
                let aliases = if member {
                    Vec::new()
                } else {
                    files
                        .fragments
                        .get(frag.path())
                        .map_or_else(Vec::new, |f| module_aliases(f, stem))
                };
                let receiver = if member {
                    Receiver::Any
                } else {
                    Receiver::Module {
                        stem,
                        aliases: &aliases,
                    }
                };
                mentions(frag, symbol, &receiver)
            } {
                Err("no_mention_in_code")
            } else if !references_module(frag, &files, symbol, core.path()) {
                Err("no_module_reference")
            } else {
                Ok(None)
            }
        };
        // A definition no cell reads has no resolved use to list: the files of
        // its group that spell the name are the text level's answer, each
        // fragment that names it in code a candidate and never a certainty. The
        // account below says what the search could not read. A name defined all
        // over the language says the same thing on every definition ("too common
        // to trace"), so nothing is searched for it: a text row per definition
        // would be the noise that line folds.
        if py.is_none() && !ambiguous {
            let _t = diffctx_base::profile::detail("impact.callers.text", "");
            let names = symbol.map_or_else(Vec::new, names_of);
            for path in spelling_files(state, &cells, &rel(state, core.path()), &names) {
                let Some(frags) = files
                    .fragments
                    .get(diffctx_base::paths::join_native(&state.root_dir, &path).as_str())
                else {
                    continue;
                };
                for &frag in frags.iter().filter(|f| !f.kind.is_signature()) {
                    let src = &frag.id;
                    if inside_the_diff(src) {
                        in_diff.insert(src.clone());
                    }
                    let reason = if within(src, core) {
                        "own_body"
                    } else {
                        match judge(frag) {
                            // A caller inside the diff is dropped after the search:
                            // it decides only whether the definition reads as
                            // called (#552).
                            Ok(found) => {
                                sources.push((frag, 0.0, found));
                                continue;
                            }
                            Err(reason) => reason,
                        }
                    };
                    *dropped.entry(reason).or_default() += 1;
                }
            }
        }
        let kept = sources.len();
        sources.retain(|(f, _, _)| !in_diff.contains(&f.id));
        let mut called_in_diff = sources.len() < kept;
        if called_in_diff {
            dropped.insert("inside_the_diff", kept - sources.len());
        }
        let mut also_changed: Vec<String> = Vec::new();
        let mut possibly_also_changed: Vec<String> = Vec::new();
        // Uses on lines no fragment owns keep the resolver's own row.
        let mut loose: Vec<Caller> = Vec::new();
        for u in verdict.iter().flat_map(|v| v.uses.iter()) {
            if u.relation == diffctx_resolve::resolve::Relation::Reexports && !u.also_changed {
                loose.push(cell_row(u));
                continue;
            }
            let owner = files
                .fragments
                .get(diffctx_base::paths::join_native(&state.root_dir, &u.file).as_str())
                .and_then(|frags| cell_caller(frags, u.line));
            if owner.is_some_and(|frag| within(&frag.id, core)) {
                continue;
            }
            let frag = owner.filter(|frag| !owned_by_a_nested_definition(frag, u));
            if u.also_changed {
                called_in_diff = true;
                let users = if u.standing == Standing::Resolved {
                    &mut also_changed
                } else {
                    &mut possibly_also_changed
                };
                users.push(match frag {
                    Some(frag) => {
                        let owner = lifted_caller(file_of(frag), frag);
                        place(&u.file, owner.symbol_name.as_deref(), &lines_of(&owner.id))
                    }
                    None => place(&u.file, None, &u.line.to_string()),
                });
            } else if let Some(frag) = frag {
                sources.push((frag, FINDING_WEIGHT, Some(reach_of(u))));
            } else {
                loose.push(cell_row(u));
            }
        }
        also_changed.sort();
        also_changed.dedup();
        possibly_also_changed.sort();
        possibly_also_changed.dedup();
        // A resolved caller inside the diff is a caller: the framework route a
        // definition nothing calls may take is not claimed over it. A possible one
        // proves nothing, and the route stands.
        let resolved_in_diff = !also_changed.is_empty();
        // A pytest fixture is called by the runner, for every test or fixture
        // that names it as a parameter — in its own file or under the
        // directory of the conftest that defines it (#401).
        if let Some(name) = symbol.filter(|_| is_pytest_fixture(core)) {
            let path = rel(state, core.path());
            let listed: FxHashSet<FragmentId> =
                sources.iter().map(|(f, _, _)| f.id.clone()).collect();
            let users = tests.fixture_askers(cells.of(&path), &path, core, base_symbol(name));
            for (frag, line) in users.into_iter().filter(|(f, _)| !listed.contains(&f.id)) {
                if inside_the_diff(&frag.id) {
                    called_in_diff = true;
                    continue;
                }
                sources.push((
                    frag,
                    FINDING_WEIGHT,
                    Some(Reach {
                        standing: Standing::Resolved,
                        relation: "reference",
                        kind: None,
                        fold: None,
                        lines: vec![line],
                    }),
                ));
            }
        }
        // A metric the definition registers is read by name in the rules that
        // query it: no import or path ties the two, the key table does.
        let path = rel(state, core.path());
        if state
            .run
            .source()
            .read_to_string(&state.root_dir.join(&path))
            .is_some_and(|text| diffctx_graph::keytable::registers_metrics(&text))
        {
            let span = core.start_line() as usize..=core.end_line() as usize;
            for reader in metric_readers
                .iter()
                .filter(|c| rel(state, &c.changed_path) == path && span.contains(&c.changed_line))
            {
                let frags = files.fragments.get(
                    diffctx_base::paths::join_native(&state.root_dir, &rel(state, &reader.path))
                        .as_str(),
                );
                if let Some(frag) = frags.and_then(|f| cell_caller(f, reader.line as u32)) {
                    sources.push((
                        frag,
                        FINDING_WEIGHT,
                        Some(Reach {
                            standing: Standing::Candidate(KEY_MATCH),
                            relation: "reference",
                            kind: Some(vocabulary::RelationKind::Reads),
                            fold: None,
                            lines: vec![reader.line as u32],
                        }),
                    ));
                }
            }
        }
        // A client that requests the path a route serves reaches its handler
        // through HTTP: no import or name ties them, only the path text does.
        for (reader, line) in super::runs::route_readers(state, &cells, diff_range, core) {
            let frags = files
                .fragments
                .get(diffctx_base::paths::join_native(&state.root_dir, &reader).as_str());
            if let Some(frag) = frags.and_then(|f| cell_caller(f, line)) {
                sources.push((
                    frag,
                    FINDING_WEIGHT,
                    Some(Reach {
                        standing: Standing::Candidate(KEY_MATCH),
                        relation: "call",
                        kind: Some(vocabulary::RelationKind::Calls),
                        fold: None,
                        lines: vec![line],
                    }),
                ));
            }
        }
        // A local `const items = rows(xs)` is a line of its function, not a
        // caller of its own: the function is, with its test link (#452).
        // An anonymous arrow (`.map((p) => …)`) is a line of the definition
        // around it as well (#520), and a field of the class holding it. So
        // is a chunk of a long function (`main[88]`), which no test edge
        // reaches: its calls arrive at the function (#579).
        for source in &mut sources {
            source.0 = lifted_caller(file_of(source.0), source.0);
        }
        // Two lines lifted into one function are one caller with both sites.
        let mut lifted: Vec<(&Fragment, f64, Option<Reach>)> = Vec::with_capacity(sources.len());
        for (frag, weight, found) in sources {
            match lifted.iter_mut().find(|(f, _, _)| f.id == frag.id) {
                Some((_, w, known)) => {
                    *w = w.max(weight);
                    match (known.as_mut(), found) {
                        (Some(k), Some(f)) => {
                            if f.standing == Standing::Resolved {
                                k.standing = Standing::Resolved;
                            }
                            // Two rows of different fold stay as listed as the
                            // one the table does not fold.
                            if k.fold != f.fold {
                                k.fold = None;
                            }
                            k.lines.extend(f.lines);
                            k.lines.sort_unstable();
                            k.lines.dedup();
                        }
                        (None, f) => *known = f,
                        _ => {}
                    }
                }
                None => lifted.push((frag, weight, found)),
            }
        }
        sources = lifted;
        sources.retain(|(f, _, _)| !within(&f.id, core));
        // The module's own guard and a registration call run the definition;
        // they are no callers of it.
        let entered = symbol.and_then(|name| {
            let name = base_symbol(name);
            file_of(core)
                .iter()
                .find_map(|f| entered_by(f, name))
                .or_else(|| {
                    let (key, value, _) = state.all_fragments.iter().find_map(|f| {
                        super::runs::dict_registrations_of(state, &cells, f, core, name)
                            .into_iter()
                            .next()
                    })?;
                    Some(Entered::Registered(format!("\"{key}\": {value}")))
                })
                .or_else(|| {
                    sources
                        .is_empty()
                        .then(|| super::php::registered_by(state, diff_range, core, file_of(core)))
                        .flatten()
                        .map(Entered::Registered)
                })
        });
        // A dict entry is no call: the fragment that holds it stays a caller
        // for a use on any other line (a dispatcher that also calls the handler).
        if let Some(name) = symbol.filter(|_| entered.is_some()) {
            let name = base_symbol(name);
            sources.retain(|(f, _, reach)| {
                let entries = super::runs::dict_registrations_of(state, &cells, f, core, name);
                (f.path() != core.path() || entered_by(f, name).is_none())
                    && (entries.is_empty()
                        || reach.as_ref().is_none_or(|r| {
                            r.lines
                                .iter()
                                .any(|l| !entries.iter().any(|(_, _, at)| at == l))
                        }))
            });
        }
        let kept = sources.len();
        // The facts layer already told a caller changed in the range from one
        // that is not: a fragment overlapping a hunk is not that.
        sources.retain(|(f, _, _)| py.is_some() || !inside_the_diff(&f.id));
        called_in_diff = called_in_diff || sources.len() < kept;
        if !dropped.is_empty() {
            tracing::debug!(
                "impact {}::{}: kept {}, dropped {:?}",
                rel(state, core.path()),
                symbol.unwrap_or("?"),
                sources.len(),
                dropped
            );
        }
        let weights: FxHashMap<&FragmentId, f64> =
            sources.iter().map(|(f, w, _)| (&f.id, *w)).collect();
        let findings: FxHashMap<&FragmentId, Reach> = sources
            .iter()
            .filter_map(|(f, _, finding)| finding.clone().map(|x| (&f.id, x)))
            .collect();
        // A fragment that reads the definition one way and calls it another
        // has a row for each kind the cell states; with no kind stated it is
        // one row, as the finding above says.
        let mut other_kinds: FxHashMap<&FragmentId, Vec<(&'static str, Reach)>> =
            FxHashMap::default();
        for (frag, _, finding) in &sources {
            let Some((finding, kind)) = finding.as_ref().and_then(|finding| {
                Some((finding, finding.stated_kind(&rel(state, frag.path()))?))
            }) else {
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
        let in_time = deadline.is_none_or(|d| Instant::now() < d);
        let commits = match (&bounds, i < MAX_BLAMED_SYMBOLS && in_time) {
            (Some((base, head)), true) => {
                let _t = diffctx_base::profile::detail("impact.callers.commits", "");
                commits_touching(state, base, head, core)
            }
            _ => 0,
        };
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
            None if is_tool(core) => tool_contract_changed(state, &base_texts, core),
            None => {
                is_public(core.path(), symbol, without_decorators(&core.content))
                    && super::rust::module_is_public(&state.run.source(), core.path())
                    && py.map_or_else(
                        || signature_changed(state, &base_texts, core),
                        |def| def.api,
                    )
            }
        };
        // A test's declarations are no API: nothing but its runner calls
        // them (#517).
        if query.is_none() && public_change && !tests.is_test(core) {
            public_api.push((
                callers.is_empty(),
                Contract {
                    path: rel(state, core.path()),
                    symbol: core.symbol_name.clone(),
                    kind: "public_api",
                },
            ));
        }
        // A changed symbol nothing depends on is the diff itself; the reader
        // holds that already, and listing it spends the cap on nothing.
        // A queried symbol's definition is half the answer: where it is.
        // Unless nothing could be resolved: an empty list is then no finding,
        // and silence would read as one. A trait method is reached through
        // its trait, whatever else shares its name (`fmt` in `impl Display`).
        // A data field or a class is no call target a framework dispatches to.
        let dispatchable = py.is_none_or(|def| {
            matches!(
                def.kind,
                diffctx_facts::facts::DefKind::Method | diffctx_facts::facts::DefKind::Property
            )
        });
        let route = (callers.is_empty() && !resolved_in_diff && dispatchable)
            .then(|| {
                dispatch_route(state, core, file_of(core)).or_else(|| {
                    verdict
                        .as_ref()
                        .and_then(decide::verdict::Verdict::overridden_via)
                        .map(str::to_string)
                })
            })
            .flatten();
        // The role that gives the definition an implicit caller: where a
        // static test link exists it is shown, otherwise the role is named.
        let test_link = |frag| {
            let found = tests.tested_by(frag, state, &by_id, &link);
            (!found.is_empty()).then(|| super::text::reachable_from_tests(&found))
        };
        let mut linkless_role = false;
        // A test helper the range's own tests call is traced like any definition:
        // the runner enters only what no one calls.
        let entry: Option<String> = if tests.is_test(core) && also_changed.is_empty() {
            Some("test (run by the test runner)".to_string())
        } else if matches!(entered, Some(Entered::Script)) {
            Some("run as a script (if __name__ == \"__main__\")".to_string())
        } else if let Some(by) = registration(core).or_else(|| match &entered {
            Some(Entered::Registered(call)) => Some(call.clone()),
            _ => None,
        }) {
            Some(
                match (by.starts_with(framework::ENTRY_PREFIX), test_link(core)) {
                    (true, Some(link)) => format!("{by}; {link}"),
                    (true, None) => by,
                    (false, Some(link)) => format!("registered by `{by}`; {link}"),
                    (false, None) => format!(
                        "registered by `{by}` \u{2014} reached through the framework, not traced"
                    ),
                },
            )
        } else if let Some(base) = http_handler_base(core, file_of(core)) {
            // The class link is modelled: no test reaching it is a finding,
            // not an unmodelled gap, so the role does not excuse it.
            let link = enclosing_class(core, file_of(core)).and_then(test_link);
            Some(match link {
                Some(link) => format!("dispatched by `{base}`; {link}"),
                None => {
                    linkless_role = true;
                    format!("dispatched by `{base}`")
                }
            })
        } else if is_program_main(core) {
            Some(match test_link(core) {
                Some(link) => format!("program entry point; {link}"),
                // A test that runs the binary is modelled for Rust: none
                // reaching `main` is a finding there, not an unmodelled gap.
                None if core.path().ends_with(".rs") => {
                    linkless_role = true;
                    "program entry point".to_string()
                }
                None => "program entry point".to_string(),
            })
        } else {
            None
        };
        let unresolved = symbol
            .filter(|_| ambiguous)
            .map(|s| Unknown::Ambiguous {
                name: s.to_string(),
                definitions: definitions
                    .get(&(core_lang, s.to_lowercase()))
                    .copied()
                    .unwrap_or(0),
                language: core_lang,
            })
            .or_else(|| route.clone().map(|via| Unknown::Overridable { via }))
            .or_else(|| {
                // The rows a text search lists are no resolution: the gap stands
                // beside them, and an empty list is never read as no caller.
                (py.is_none() && is_code_file(core.path())).then(|| Unknown::NotModelled {
                    language: if core_lang.is_empty() {
                        "this language"
                    } else {
                        core_lang
                    },
                })
            });
        // An entry point is called by its runner or framework: the hedge for
        // callers nothing traces would be a false unknown.
        let unresolved = unresolved.filter(|u| {
            entry.is_none()
                || !matches!(
                    u,
                    Unknown::Overridable { .. }
                        | Unknown::NotModelled { .. }
                        | Unknown::Dispatched { .. }
                )
        });
        // What the resolver could not land is said, never read as no caller.
        let shown = py.map_or_else(
            || symbol.map(base_symbol).unwrap_or("").to_string(),
            |def| def.name.clone(),
        );
        // A route no model traces is named by the answer's own reason when it has one.
        let routed = matches!(unresolved, Some(Unknown::Overridable { .. }));
        let unresolved = verdict
            .as_ref()
            .and_then(decide::verdict::Verdict::gap)
            .filter(|gap| {
                !(routed
                    && matches!(
                        gap,
                        decide::gap::Gap::NotTraced
                            | decide::gap::Gap::PassedAsValue { .. }
                            | decide::gap::Gap::ImplicitlyConstructed
                    ))
            })
            .map(|gap| cell_unknown(&gap, &shown))
            .or(unresolved);
        // Read after the search: a stop noted while it ran, or before it
        // began, leaves its callers a lower bound.
        let stops = state.run.stops();
        let unresolved = if stops.is_empty() {
            unresolved
        } else {
            Some(Unknown::Unfinished { stops })
        };
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
            || class.is_some_and(|class| !tests.tested_by(class, state, &by_id, &link).is_empty());
        let tests_absent = query.is_none()
            && tests.any()
            && !tests.is_test(core)
            && (entry.is_none() || linkless_role)
            && !callers
                .iter()
                .any(|c| c.tests.iter().any(|t| t.via.is_none()))
            && tests.tested_by(core, state, &by_id, &link).is_empty()
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
            ) && (!name.contains('.')
                || matches!(extension(name).as_str(), "yml" | "yaml" | "rake"))
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
        let unsearched = nothing_known
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
        untested_definitions += usize::from(absent);
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
                && (class_tested() || !tests.tested_by(core, state, &by_id, &link).is_empty())
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
                traced_definitions -= 1;
                continue;
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
                    untested_called_in_diff.push(named);
                } else {
                    untested_unlisted.push(absence(named, complete));
                }
            } else if let Some(role) = entry.as_ref() {
                entry_unlisted.push(format!("{named} \u{2014} {role}"));
            } else if called_in_diff {
                untested_called_in_diff.push(named);
            } else {
                // No test is in view to claim a missing link against: the search
                // still found no caller, and the definition is named for that.
                untested_unlisted.push(absence(named, complete));
            }
            continue;
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
            untested_called_in_diff.push(core_place(core));
        }
        changed.push(ChangedSymbol {
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
    drop(callers);
    cell_untracked.sort_by(|a, b| (a.path(), a.start_line()).cmp(&(b.path(), b.start_line())));
    cell_untracked.dedup_by(|a, b| a.path() == b.path() && a.start_line() == b.start_line());
    for core in cell_untracked {
        let name = core.symbol_name.as_deref().map_or("", base_symbol);
        changed.push(ChangedSymbol {
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
            changed.push(uncovered_symbol(
                path.as_str().to_string(),
                name,
                String::new(),
                "function".to_string(),
                reason,
            ));
        }
    }
    for (core, reason) in cell_unread {
        changed.push(uncovered_symbol(
            rel(state, core.path()),
            core.symbol_name.as_deref().map_or("", base_symbol),
            lines_of(&core.id),
            format!("{:?}", core.kind).to_lowercase(),
            reason,
        ));
    }
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
    for entry in &cell_apart {
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
            layout_only.push(place(
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
        traced_definitions += 1;
        traced_names.push(entry.name.clone());
        // A definition with no fragment has no test link computed: with tests in
        // view it keeps its row, which claims none; the list that says "no static
        // test link" is for the answer that has no tests to say it against.
        let named = place(&sym.path, sym.symbol.as_deref(), &sym.lines);
        let in_a_test_file = tests.is_test_file(&sym.path);
        let witness = sym.coverage.all().witness();
        match witness.filter(|_| callerless && verdict.complete()) {
            Some(_) if in_a_test_file => {
                entry_unlisted.push(format!("{named} \u{2014} test (run by the test runner)"));
            }
            Some(complete) if !tests.any() => untested_unlisted.push(absence(named, complete)),
            _ => changed.push(sym),
        }
    }
    diffctx_base::profile::count("impact.traced_apart", traced_apart);
    {
        let stops = state.run.stops();
        for entry in apart_unreached {
            changed.push(unfinished_symbol(
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
                changed.push(unfinished_symbol(
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
                changed.push(unfinished_symbol(
                    path,
                    None,
                    String::new(),
                    "file".to_string(),
                    &stops,
                ));
            }
        }
    }
    let removed = diffctx_base::profile::phase("impact.removed");
    if query.is_none() && state.run.check() {
        let mut defined: FxHashMap<&str, FxHashSet<String>> = FxHashMap::default();
        for (n, p) in &defined_in {
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
                let tested = tests.tested_by(frag, state, &by_id, &link);
                c.file_tested = tested.is_empty() && tests.file_tested(frag, &reaches_file);
                c.tests_open = tested.is_empty() && tests.open_above(frag, &link, true);
                c.tests_partial =
                    !tested.is_empty() && tests.nearest_only(frag, state, &by_id, &link);
                c.tests = tested;
            }
            if let Some(c) =
                contract.filter(|c| !diffctx_base::testfiles::is_test_path(Path::new(&c.path)))
            {
                contracts.push(c);
            }
            // Unresolved is an answer too: dropped, it read as no caller.
            let alone = sym
                .symbol
                .as_ref()
                .is_some_and(|name| listed_alone.contains(&(sym.path.clone(), name.clone())));
            if !sym.callers.is_empty() || sym.unresolved.is_some() || alone {
                changed.push(sym);
            }
        }
    }
    drop(removed);
    // Symbols with something to say first: callers, then commit overlap.
    changed.sort_by(|a, b| {
        b.callers
            .len()
            .cmp(&a.callers.len())
            .then_with(|| b.commits.cmp(&a.commits))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.lines.cmp(&b.lines))
    });

    let changed_files: Vec<String> = changed_rels(state).collect();
    let layout_files = layout_files(state);
    // A queried symbol changes nothing: the file holding its definition is
    // no contract that moved.
    for path in changed_files.iter().filter(|_| query.is_none()) {
        if is_schema_file(path) && !layout_files.contains(path.as_str()) {
            contracts.push(Contract {
                path: path.clone(),
                symbol: None,
                kind: "schema",
            });
        }
    }

    public_api.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.path.cmp(&b.1.path)));
    contracts.extend(public_api.into_iter().map(|(_, c)| c));
    let contracts_omitted = contracts.len().saturating_sub(MAX_CONTRACTS);
    contracts.truncate(MAX_CONTRACTS);
    // Pack's rule, not a list of our own: a run-wide stop, or a per-file
    // limit on a changed file. The graph's per-node cap is routine and stays
    // out; the callers it cut an edge from are recovered per definition.
    let changed_set: FxHashSet<&str> = changed_files.iter().map(String::as_str).collect();
    let untracked: FxHashSet<String> = state
        .untracked_files
        .iter()
        .map(|p| rel(state, &p.to_string_lossy()))
        .collect();
    let (stale, stale_omitted) = if query.is_none() {
        let _stale = diffctx_base::profile::phase("impact.stale_paths");
        stale_references(state, &cells, analysed)
    } else {
        (Vec::new(), 0)
    };
    let (deleted_modules, changed) = if query.is_none() {
        dead_modules(state, &cells, changed, &stale)
    } else {
        (Vec::new(), changed)
    };
    let (named_by, named_by_path_omitted) = {
        let _named = diffctx_base::profile::phase("impact.named_by");
        let (mut named, omitted) = named_by_path(state, &cells, analysed);
        if query.is_none() {
            let by_module = untraced.module_refs(state, &cores, &named);
            named.extend(by_module);
            named.extend(twin_references(state, &cores));
        }
        (named, omitted)
    };
    let (collection_reads, collection_reads_omitted) = if query.is_none() {
        read_by_collection(state, analysed)
    } else {
        (Vec::new(), 0)
    };
    let nameless = if query.is_none() {
        nameless_changes(state, &cells, &held)
    } else {
        Vec::new()
    };
    let (mut unanalysed, cell_read_configs, consumed_outside, lost_configs) = if query.is_none() {
        // A file the run did not analyse whole has its own reason, which already
        // says its effect is unknown: it is never listed twice. The rest come
        // first, so that the cap on the list keeps them.
        let mut others = unanalysed_files(state, &changed_files);
        // A base side over the size cap is never parsed: its removals go unread,
        // and the file is named at the path the answer reports (the head's, for
        // a rename), whether it changed, was renamed or was deleted.
        let old_path_of: FxHashMap<&str, &str> = state
            .renamed_files
            .iter()
            .map(|(old, new)| (new.as_str(), old.as_str()))
            .collect();
        let sides = changed_files
            .iter()
            .map(|p| {
                (
                    old_path_of.get(p.as_str()).copied().unwrap_or(p.as_str()),
                    p.as_str(),
                )
            })
            .chain(state.deleted_files.iter().map(|p| (p.as_str(), p.as_str())));
        for (old, new) in sides {
            let over = |path: &str| {
                cells.reading(path).and_then(|cell| cell.loss_about(path))
                    == Some(decide::gap::Reason::TooLarge)
            };
            if (over(old) || over(new)) && !others.iter().any(|f| f.path == new) {
                others.push(UnanalysedFile::new(
                    new.to_string(),
                    "too_large",
                    Vec::new(),
                ));
            }
        }
        // A changed file a cell read whole is traced: what it holds outside every
        // definition is `outside_definitions`, and nothing else of it is untraced.
        // What the code reads of its keys is no cell's answer yet: the key search
        // still runs for it, as it did before the cell read it.
        let cell_read: Vec<String> = others
            .iter()
            .filter(|f| {
                f.reason == "content_untraced"
                    && cells.of(&f.path).is_some_and(|cell| cell.is_bound(&f.path))
            })
            .map(|f| f.path.clone())
            .collect();
        // The cell traces what is inside the files; what builds or applies them
        // lies outside, and a mention or a reader in another file is all that
        // reaches it. Such a file is a gap again unless one is found below.
        let consumed_outside: Vec<UnanalysedFile> = others
            .iter()
            .filter(|f| {
                cell_read.contains(&f.path)
                    && cells
                        .of(&f.path)
                        .is_some_and(decide::cell::Cell::consumed_outside)
            })
            .cloned()
            .collect();
        others.retain(|f| !cell_read.contains(&f.path));
        // A file its cell lost, in part or whole, is not made traced by the key
        // search that still runs for it: its loss is what the answer says of it.
        let mut lost_configs: Vec<String> = Vec::new();
        for f in &mut others {
            let lost = (f.reason == "content_untraced")
                .then(|| {
                    cells
                        .reading(&f.path)
                        .and_then(|cell| cell.loss_about(&f.path))
                })
                .flatten();
            if let Some(reason) = lost {
                f.reason = reason.row_reason();
                lost_configs.push(f.path.clone());
            }
        }
        let mut all: Vec<UnanalysedFile> = outside_definitions(&cells)
            .into_iter()
            .filter(|o| !others.iter().any(|f| f.path == o.path))
            .collect();
        all.extend(others);
        for n in &nameless {
            if !all.iter().any(|f| f.path == n.path) {
                all.push(UnanalysedFile {
                    language: diffctx_base::languages::get_language_for_file(&n.path),
                    path: n.path.clone(),
                    reason: OUTSIDE_DEFINITIONS,
                    lines: n
                        .runs
                        .iter()
                        .take(MAX_OUTSIDE_RUNS)
                        .flat_map(|&(first, last)| [first, last])
                        .collect(),
                });
            }
        }
        (all, cell_read, consumed_outside, lost_configs)
    } else {
        (Vec::new(), Vec::new(), Vec::new(), Vec::new())
    };
    let mut named_by = named_by;
    let (key_searches, key_files_unsearched, named_by_key_omitted) = {
        let untraced: Vec<String> = unanalysed
            .iter()
            .filter(|f| f.reason == "content_untraced")
            .map(|f| f.path.clone())
            .chain(cell_read_configs)
            .chain(lost_configs.iter().cloned())
            .chain(nameless.iter().map(|n| n.path.clone()))
            .chain(changed_files.iter().filter(|p| is_schema_file(p)).cloned())
            .collect();
        let (refs, omitted, searches, unsearched, unread) =
            key_references(state, &cells, &untraced, &nameless);
        unanalysed.extend(unread);
        // A search of the names a nameless change touches is no trace of it.
        unanalysed.retain(|f| {
            lost_configs.contains(&f.path)
                || nameless.iter().any(|n| n.path == f.path)
                || !searches.iter().any(|k| k.path == f.path)
        });
        named_by.extend(refs);
        (searches, unsearched, omitted)
    };
    // A config or a Dockerfile nothing names stays a gap: its builds are not
    // traced, and a clean zero would claim they are.
    for file in consumed_outside {
        let read = named_by.iter().any(|r| r.target == file.path)
            || changed
                .iter()
                .any(|c| c.path == file.path && c.callers.iter().any(|k| k.path != file.path));
        if !read {
            unanalysed.push(file);
        }
    }
    // Read after the last phase that can stop: a deadline the stale-reference
    // search hit was missing from an answer that had dropped its lines. A
    // changed file outside code holds no definition and calls nothing: one
    // over the cap is named in `unanalysed`, and no caller is missing for it
    // (#626).
    let (mut limits, _, _) = diffctx_snapshot::resource::classify_limits(&state.run, &[], &|p| {
        changed_set.contains(p) && (is_code_file(p) || Path::new(p).extension().is_none())
    });
    // A changed file cut to the fragment cap keeps every fragment the diff
    // touched: pack loses context there, impact loses no changed definition,
    // and the cap fires on most large files (#306).
    limits.retain(|r| *r != diffctx_snapshot::resource::LimitReason::FragmentLimit);
    // A symbol lookup reads the files naming the symbol itself.
    let outside_scope_files = if query.is_none() {
        unread_namers(state, analysed, traced_names)
    } else {
        0
    };
    let mut output = ImpactOutput {
        name: directory_name(&state.root_dir),
        range: diff_range.map(str::to_string),
        index_tree: state.index_tree.clone(),
        symbol: query.map(str::to_string),
        untracked_files: changed_files
            .iter()
            .filter(|f| untracked.contains(f.as_str()))
            .cloned()
            .collect(),
        changed_files: changed_files
            .into_iter()
            .filter(|f| !untracked.contains(f.as_str()))
            .collect(),
        deleted_files: state.deleted_files.clone(),
        stale_references: stale,
        stale_references_omitted: stale_omitted,
        deleted_modules,
        named_by,
        named_by_path_omitted,
        named_by_key_omitted,
        key_searches,
        key_files_unsearched,
        collection_reads,
        collection_reads_omitted,
        commit_count: state.commit_count,
        changed,
        contracts,
        contracts_omitted,
        limits,
        tests_known: tests.any(),
        untested_definitions,
        untested_unlisted,
        untested_called_in_diff,
        entry_unlisted,
        layout_only,
        lockfile_changes: state.lockfile_changes.clone(),
        traced_definitions,
        withheld_files: state.policy_excluded_count + state.run.withheld_count(),
        outside_scope_files,
        unread_directories: state.run.source().unread_directories(),
        unanalysed_files: unanalysed,
        run: Some(state.run.clone()),
        cap: IMPACT_TOKEN_CAP,
        ..ImpactOutput::default()
    };
    output.empty = proves_empty(&output);
    output
}
