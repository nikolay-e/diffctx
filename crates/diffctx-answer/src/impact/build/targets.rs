//! Which definitions the range changed, and which of them the answer traces: the fragments the hunks
//! seed, the entries of the facts layer anchored on them, and the namers of what they spell.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::decide::{self};

use diffctx_base::container::{innermost, named_container_of, within};
use diffctx_base::types::{Fragment, FragmentId, FragmentKind};

use super::super::cells::{cell_anchor, is_module_block_binding};
use super::super::changes::{moved_verbatim, rel, without_decorators};
use super::super::dispatch::{base_symbol, enclosing_class, indent_of};
use super::super::symbol::parse_symbol_query;
use super::super::unanalysed::{UntracedNamers, left_words, literal_base, spelled_word};
use super::context::{Findings, Impact};

/// The definitions to trace, and what the facts layer says of the ones with no fragment of their own.
pub(super) struct Targets<'a> {
    pub(super) cores: Vec<&'a Fragment>,
    pub(super) cell_defs: FxHashMap<&'a FragmentId, decide::changed::Changed>,
    pub(super) cell_apart: Vec<decide::changed::Changed>,
    pub(super) cell_removed: Vec<decide::changed::Changed>,
    pub(super) cell_untracked: Vec<&'a Fragment>,
    pub(super) cell_unread: Vec<(&'a Fragment, decide::gap::Reason)>,
    pub(super) cell_lost: Vec<&'a Fragment>,
    pub(super) bordering: FxHashSet<&'a FragmentId>,
    pub(super) held: FxHashMap<String, Vec<(u32, u32)>>,
    pub(super) untraced: UntracedNamers,
}

pub(super) fn read<'a>(cx: &Impact<'a>, found: &mut Findings) -> Targets<'a> {
    let (state, query, cells, by_id, files, diff_range) = (
        cx.state,
        cx.query,
        cx.cells,
        &cx.by_id,
        &cx.files,
        cx.diff_range,
    );
    let file_of = |f: &Fragment| cx.file_of(f);
    let hunks_in = |core: &Fragment| cx.hunks_in(core);
    let core_place = |core: &Fragment| cx.core_place(core);
    let layout_only = &mut found.layout_only;
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
    let mut held: FxHashMap<String, Vec<(u32, u32)>> = FxHashMap::default();
    for c in &cores {
        held.entry(rel(state, c.path()))
            .or_default()
            .push((c.start_line(), c.end_line()));
    }
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
    let untraced = {
        let _t = diffctx_base::profile::detail("impact.setup.untraced", "");
        let removed: Vec<&str> = cell_removed
            .iter()
            .chain(&cell_apart)
            .filter_map(spelled_word)
            .collect();
        let own: Vec<super::super::keys::OwnSpan> = cell_defs
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
        UntracedNamers::build(state, cells, &cores, &removed, &left, &own, diff_range)
    };
    Targets {
        cores,
        cell_defs,
        cell_apart,
        cell_removed,
        cell_untracked,
        cell_unread,
        cell_lost,
        bordering,
        held,
        untraced,
    }
}
