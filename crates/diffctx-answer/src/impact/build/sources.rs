//! Who reaches a changed definition: the resolver's uses, the text search where no cell reads the
//! definition, and the readers no import ties to it (fixtures, metrics, routes).

use rustc_hash::{FxHashMap, FxHashSet};

use crate::answer::render::place;

use diffctx_base::container::within;

use diffctx_base::types::{Fragment, FragmentId, FragmentKind};

use diffctx_facts::parsers::code_text;

use diffctx_occurrence::tokens::occurrence::Standing;

use diffctx_resolve::vocabulary;

use super::super::callers::{
    Receiver, binds_locally, is_code_file, is_import_block, mentions, module_aliases,
    references_module,
};
use super::super::cells::{
    FINDING_WEIGHT, KEY_MATCH, Reach, cell_caller, cell_row, lifted_caller,
    owned_by_a_nested_definition, reach_of,
};
use super::super::changes::{lines_of, names_of, rel, same_runtime, spelling_files};
use super::super::dispatch::base_symbol;
use super::super::entry::{Entered, entered_by, is_pytest_fixture};
use super::super::output::Caller;

use super::context::Impact;
use super::definition::Core;
use super::targets::Targets;

pub(super) struct Sources<'a> {
    pub(super) sources: Vec<(&'a Fragment, f64, Option<Reach>)>,
    pub(super) also_changed: Vec<String>,
    pub(super) possibly_also_changed: Vec<String>,
    pub(super) loose: Vec<Caller>,
    pub(super) called_in_diff: bool,
    pub(super) entered: Option<Entered>,
}

pub(super) fn of<'a>(cx: &Impact<'a>, t: &Targets<'a>, c: &Core<'a, '_>) -> Sources<'a> {
    let state = cx.state;
    let query = cx.query;
    let cells = cx.cells;
    let tests = &cx.tests;
    let files = &cx.files;
    let defined_in = &cx.defined_in;
    let analysed = cx.analysed;
    let bordering = &t.bordering;
    let in_a_hunk = |id: &FragmentId| cx.in_a_hunk(id);
    let one_tree = |user: &str, used: &str| cx.one_tree(user, used);
    let file_of = |f: &Fragment| cx.file_of(f);
    let inside_the_diff = |id: &FragmentId| {
        (state.core_ids.contains(id) && !bordering.contains(id))
            || (query.is_none() && in_a_hunk(id))
    };
    let core = c.core;
    let py = c.py;
    let verdict = &c.verdict;
    let symbol = c.symbol;
    let member = c.member;
    let stem = c.stem;
    let binding_model = c.binding_model;
    let ambiguous = c.ambiguous;
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
        } else if !references_module(frag, files, symbol, core.path()) {
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
    if py.is_none() && !ambiguous && c.class_use.is_none() {
        let _t = diffctx_base::profile::detail("impact.callers.text", "");
        let names = symbol.map_or_else(Vec::new, names_of);
        for path in spelling_files(state, cells, &rel(state, core.path()), &names) {
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
    let mut loose: Vec<Caller> = c.class_use.iter().flat_map(|u| u.readers.clone()).collect();
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
    // A pytest fixture is called by the runner, for every test or fixture
    // that names it as a parameter — in its own file or under the
    // directory of the conftest that defines it (#401).
    if let Some(name) = symbol.filter(|_| is_pytest_fixture(core)) {
        let path = rel(state, core.path());
        let listed: FxHashSet<FragmentId> = sources.iter().map(|(f, _, _)| f.id.clone()).collect();
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
    for (reader, line) in super::super::runs::route_readers(state, cells, analysed, core) {
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
                    super::super::runs::dict_registrations_of(state, cells, f, core, name)
                        .into_iter()
                        .next()
                })?;
                Some(Entered::Registered(format!("\"{key}\": {value}")))
            })
            .or_else(|| {
                sources
                    .is_empty()
                    .then(|| super::super::php::registered_by(state, analysed, core, file_of(core)))
                    .flatten()
                    .map(Entered::Registered)
            })
    });
    // A dict entry is no call: the fragment that holds it stays a caller
    // for a use on any other line (a dispatcher that also calls the handler).
    if let Some(name) = symbol.filter(|_| entered.is_some()) {
        let name = base_symbol(name);
        sources.retain(|(f, _, reach)| {
            let entries = super::super::runs::dict_registrations_of(state, cells, f, core, name);
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
    Sources {
        sources,
        also_changed,
        possibly_also_changed,
        loose,
        called_in_diff,
        entered,
    }
}
