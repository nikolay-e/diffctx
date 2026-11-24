//! One changed definition as the trace reads it: its facts-layer entry and verdict, its layout and
//! bordering rules, and whether its name is too common to trace by text.

use std::path::Path;

use crate::decide::{self};

use diffctx_base::types::Fragment;

use super::super::MAX_DEFINITIONS;
use super::super::callers::is_member;
use super::super::changes::{moved_verbatim, names_of};
use super::super::entry::compatible_extension;

use super::context::{Findings, Impact};
use super::targets::Targets;

/// What the later phases ask of the definition being traced.
pub(super) struct Core<'a, 't> {
    pub(super) core: &'a Fragment,
    pub(super) py: Option<&'t decide::changed::Changed>,
    pub(super) verdict: Option<decide::verdict::Verdict>,
    pub(super) compatible: bool,
    pub(super) symbol: Option<&'a str>,
    pub(super) member: bool,
    pub(super) stem: &'a str,
    pub(super) core_lang: &'static str,
    pub(super) binding_model: bool,
    pub(super) ambiguous: bool,
}

/// `None` when the definition is layout only, or only borders a deleted one: nothing is traced for it.
pub(super) fn read<'a, 't>(
    cx: &Impact<'a>,
    t: &'t Targets<'a>,
    core: &'a Fragment,
    found: &mut Findings,
) -> Option<Core<'a, 't>> {
    let state = cx.state;
    let query = cx.query;
    let cells = cx.cells;
    let definitions = &cx.definitions;
    let bordering = &t.bordering;
    let cell_defs = &t.cell_defs;
    let hunks_in = |core: &Fragment| cx.hunks_in(core);
    let core_place = |core: &Fragment| cx.core_place(core);
    let file_of = |f: &Fragment| cx.file_of(f);
    let core_hunks = hunks_in(core);
    let py = cell_defs.get(&core.id);
    let verdict = py.and_then(|def| {
        let cell = cells.of(&def.file)?;
        let _t = diffctx_base::profile::detail("impact.callers.verdict", "");
        Some(cell.verdict(def))
    });
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
            (matches!(def.how, decide::changed::How::Edited { layout_only: true }) || hunk_layout)
                && !def.not_layout
        }
        None => hunk_layout,
    };
    if query.is_none() && layout {
        found.layout_only.push(core_place(core));
        return None;
    }
    if query.is_none() && py.is_none() && bordering.contains(&core.id) {
        return None;
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
    Some(Core {
        core,
        py,
        verdict,
        compatible,
        symbol,
        member,
        stem,
        core_lang,
        binding_model,
        ambiguous,
    })
}
