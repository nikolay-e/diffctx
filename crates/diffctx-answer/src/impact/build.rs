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

use diffctx_base::container::{innermost, within};

use diffctx_base::types::{Fragment, FragmentId, FragmentKind};

use diffctx_facts::facts::framework;

use diffctx_facts::parsers::code_text;

use diffctx_occurrence::tokens::occurrence::Standing;

use diffctx_resolve::vocabulary;

use super::tests_reach::TestsReach;

use super::MAX_BLAMED_SYMBOLS;
use super::MAX_DEFINITIONS;
use super::callers::{
    Receiver, binds_locally, extension, first_naming_line, is_code_file, is_import_block,
    is_member, mentions, module_aliases, references_module,
};
use super::cells::{
    FINDING_WEIGHT, KEY_MATCH, REEXPORT, Reach, cell_caller, cell_row, cell_unknown,
    held_has_users, lifted_caller, owned_by_a_nested_definition, reach_of,
};
use super::changes::{
    commits_touching, is_public, lines_of, moved_verbatim, names_of, rel, same_runtime,
    signature_changed, spelling_files, tool_contract_changed, without_decorators,
};
use super::coverage::{absence, search_coverage};
use super::dispatch::{base_symbol, dispatch_route, enclosing_class, merge_by_symbol};
use super::entry::{
    Entered, compatible_extension, entered_by, http_handler_base, is_program_main,
    is_pytest_fixture,
};
use super::output::{Caller, ChangedSymbol, Contract, ImpactOutput, UnanalysedFile, Unknown};
use super::unanalysed::{
    MAX_UNTRACED_NAMERS, callable_by_name, later_needles, left_words, searched_as_text,
    spelled_name, unaccounted_rows,
};

mod apart;
mod context;
mod gaps;
mod range;
mod removals;
mod targets;

use context::{Findings, Impact};
use targets::Targets;

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
    let analysed = state.analysed_range.as_deref().or(diff_range);
    let cells = {
        let _t = diffctx_base::profile::detail("impact.setup.cells", "");
        Cells::open(state, analysed, query.is_some())
    };
    let by_id = context::fragments_by_id(state);
    let named = targets::named_cores(state, &by_id);
    let later = later_needles(state, &named);
    let tests = {
        let _t = diffctx_base::profile::detail("impact.setup.tests", "");
        TestsReach::build(state, &cells, analysed, &later)
    };
    let cx = Impact::open(state, &cells, tests, by_id, diff_range, query, deadline);
    let mut found = Findings::default();
    let t = targets::read(&cx, named, &mut found);
    let Targets {
        cores,
        cell_defs,
        bordering,
        untraced,
        ..
    } = &t;
    drop(setup);
    let (state, cells, tests, by_id, files) = (cx.state, cx.cells, &cx.tests, &cx.by_id, &cx.files);
    let (definitions, defined_in, bounds, base_texts) =
        (&cx.definitions, &cx.defined_in, &cx.bounds, &cx.base_texts);
    let in_a_hunk = |id: &FragmentId| cx.in_a_hunk(id);
    let hunks_in = |core: &Fragment| cx.hunks_in(core);
    let core_place = |core: &Fragment| cx.core_place(core);
    let one_tree = |user: &str, used: &str| cx.one_tree(user, used);
    let file_of = |f: &Fragment| cx.file_of(f);
    let registration = |f: &Fragment| cx.registration(f);
    let is_tool = |f: &Fragment| cx.is_tool(f);
    let link = |user: &Fragment, used: &Fragment| cx.link(user, used);
    let test_links = |frag, core: Option<&Fragment>| cx.test_links(frag, core);
    // A queried name has no diff: a call from elsewhere in its own file is a
    // caller like any other.
    let inside_the_diff = |id: &FragmentId| {
        (state.core_ids.contains(id) && !bordering.contains(id))
            || (query.is_none() && in_a_hunk(id))
    };
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
            found.layout_only.push(core_place(core));
            continue;
        }
        if query.is_none() && py.is_none() && bordering.contains(&core.id) {
            continue;
        }
        found.traced_definitions += 1;
        found
            .traced_names
            .extend(core.symbol_name.as_deref().and_then(|s| names_of(s).pop()));
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
            for reader in cx
                .metric_readers()
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
        let in_time = cx.deadline.is_none_or(|d| Instant::now() < d);
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
            found.public_api.push((
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
                found.traced_definitions -= 1;
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
