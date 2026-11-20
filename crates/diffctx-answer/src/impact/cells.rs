//! The cells of the decision as impact rows: the caller, the unknown and the symbol a cell stands for.

use std::path::Path;

use crate::answer::render::place;

use crate::decide::{self};

use diffctx_base::types::{Fragment, FragmentKind};

use diffctx_occurrence::tokens::occurrence::Standing;

use diffctx_resolve::vocabulary;

use super::MAX_REMOVED_NAME_HITS;
use super::deliver::relation_rank;
use super::dispatch::{base_symbol, merge_by_symbol};
use super::output::{Caller, ChangedSymbol, Unknown};
use super::unanalysed::unaccounted_rows;

/// The fragment that spans a changed definition: the one of its name
/// whose lines overlap the definition's and lie nearest them.
pub(super) fn cell_anchor<'a>(
    frags: &[&'a Fragment],
    def: &decide::changed::Changed,
) -> Option<&'a Fragment> {
    frags
        .iter()
        .copied()
        .filter(|f| {
            f.kind != FragmentKind::Impl
                && f.kind != FragmentKind::Excerpt
                && f.kind != FragmentKind::Chunk
                && !f.kind.is_signature()
                && f.symbol_name.as_deref().map(base_symbol) == Some(def.name.as_str())
                && f.start_line() <= def.lines.1
                && def.lines.0 <= f.end_line()
        })
        .min_by_key(|f| f.start_line().abs_diff(def.lines.0) + f.end_line().abs_diff(def.lines.1))
}

/// A `const`/`let` of a module-level block (a `for` body): a fragment the
/// facts hold no definition for, and no function around it makes it a local.
pub(super) fn is_module_block_binding(core: &Fragment, file: &[&Fragment]) -> bool {
    core.kind == FragmentKind::Variable
        && !file.iter().any(|f| {
            f.symbol_name.is_some()
                && f.kind != FragmentKind::Chunk
                && f.kind != FragmentKind::Excerpt
                && !f.kind.is_signature()
                && f.line_count() > core.line_count()
                && f.start_line() <= core.start_line()
                && core.end_line() <= f.end_line()
        })
}

/// The fragment that owns a line of use: the smallest named one around it.
pub(super) fn cell_caller<'a>(frags: &[&'a Fragment], line: u32) -> Option<&'a Fragment> {
    frags
        .iter()
        .copied()
        .filter(|f| {
            f.start_line() <= line
                && line <= f.end_line()
                && f.kind != FragmentKind::Excerpt
                && !f.kind.is_signature()
        })
        .min_by_key(|f| (f.symbol_name.is_none(), f.line_count()))
}

/// A use whose owner is a definition nested in the fragment that holds its line, by
/// the resolver's own scope. In TypeScript a function-valued constant or member that
/// leaves its enclosing definition (`got.extend = …` in a factory that returns `got`)
/// is a definition of its own, and the fragmenter cuts none for it: the smallest
/// fragment around the line is the factory, which may itself be changed. The row is
/// the nested definition's, never the factory's.
pub(super) fn owned_by_a_nested_definition(frag: &Fragment, u: &decide::verdict::Use) -> bool {
    let diffctx_facts::facts::Scope::Def(local) = &u.scope else {
        return false;
    };
    matches!(
        diffctx_base::languages::get_language_for_file(&u.file),
        Some("typescript" | "javascript")
    ) && frag
        .symbol_name
        .as_deref()
        .map(base_symbol)
        .is_some_and(|own| {
            local.name.as_str() != own && local.container.iter().any(|c| c.as_str() == own)
        })
}

/// The definition a fragment that owns a line of use is a piece of: the function
/// around a local variable, an anonymous arrow, a chunk of a long function, the
/// class holding a field, the test function around a def or class declared inside
/// its body (a test's own fixture, never a test). Any other fragment stands for
/// itself.
pub(super) fn lifted_caller<'a>(frags: &[&'a Fragment], source: &'a Fragment) -> &'a Fragment {
    let anonymous = source.kind == FragmentKind::Function && source.symbol_name.is_none();
    let field = source.kind == FragmentKind::Property;
    let chunk = source.kind == FragmentKind::Chunk;
    let in_a_test = matches!(source.kind, FragmentKind::Function | FragmentKind::Class)
        && diffctx_base::testfiles::is_test_path(Path::new(source.path()));
    if source.kind != FragmentKind::Variable && !anonymous && !field && !chunk && !in_a_test {
        return source;
    }
    let parent = source.symbol_name.as_deref().map(base_symbol);
    frags
        .iter()
        .copied()
        .filter(|f| {
            (f.kind == FragmentKind::Function
                || (anonymous && f.kind == FragmentKind::Variable)
                || (field && f.kind.is_container())
                || (chunk
                    && f.kind != FragmentKind::Chunk
                    && parent.is_none_or(|p| f.symbol_name.as_deref() == Some(p))))
                && f.symbol_name.is_some()
                && f.id != source.id
                && f.start_line() <= source.start_line()
                && f.end_line() >= source.end_line()
        })
        .min_by_key(|f| f.line_count())
        .unwrap_or(source)
}

/// What every row the resolver found weighs: the kinds of relation are not
/// ranked against each other, the cap deals the rows out among them.
pub(super) const FINDING_WEIGHT: f64 = 1.0;

/// Why a reader the typed key table found is a candidate: the name it spells is
/// the key the definition registers, and no binding says it is that definition's.
pub(super) const KEY_MATCH: &str = "key_match";

/// What the cell states of the uses one caller makes of a changed definition.
#[derive(Clone)]
pub(super) struct Reach {
    pub(super) standing: Standing,
    pub(super) relation: &'static str,
    pub(super) kind: Option<vocabulary::RelationKind>,
    /// The rule of the folding table that says the change cannot have affected
    /// the use, the relation kind it is counted under, and how many definitions
    /// share the name a candidate may be.
    pub(super) fold: Option<(&'static str, &'static str, usize)>,
    pub(super) lines: Vec<u32>,
}

impl Reach {
    pub(super) fn evidence(&self) -> &'static str {
        match self.standing {
            Standing::Resolved => "resolved",
            Standing::Candidate(_) => "candidate",
        }
    }

    pub(super) fn reason(&self) -> Option<&'static str> {
        match self.standing {
            Standing::Candidate(reason) => Some(reason),
            Standing::Resolved => None,
        }
    }

    /// The kind a row built from this states for a caller in `path`: the
    /// use's own, for a precise (language, relation) cell only. A use resolves
    /// within one language, so the caller's file names the cell of the
    /// definition it reaches.
    pub(super) fn stated_kind(&self, path: &str) -> Option<&'static str> {
        self.kind
            .and_then(|kind| vocabulary::stated_kind_of(path, kind))
    }
}

pub(super) fn reach_of(u: &decide::verdict::Use) -> Reach {
    use diffctx_resolve::resolve::Relation as Uses;
    Reach {
        standing: u.standing,
        relation: match u.relation {
            Uses::Calls | Uses::Instantiates => "call",
            Uses::Overrides => "implementation",
            _ => "reference",
        },
        kind: Some(vocabulary::kind_of_use(u.relation)),
        fold: u
            .folded
            .map(|rule| (rule.name(), u.relation.name(), u.sharing)),
        lines: vec![u.line],
    }
}

/// What the answer says of a gap in the resolver's reach: a name on more lines
/// than can be told apart is too common to trace (#413), any other count is
/// candidates, each at its site (#399).
pub(super) fn cell_unknown(gap: &decide::gap::Gap, name: &str) -> Unknown {
    match gap {
        decide::gap::Gap::Candidates { uses, .. } if *uses > MAX_REMOVED_NAME_HITS => {
            Unknown::CommonWord {
                name: name.to_string(),
                lines: *uses,
            }
        }
        decide::gap::Gap::Candidates { uses, sites } => Unknown::Candidates {
            name: name.to_string(),
            uses: *uses,
            sites: sites.clone(),
        },
        decide::gap::Gap::Unpaired => Unknown::Unpaired {
            name: name.to_string(),
        },
        decide::gap::Gap::Stopped { reasons } => Unknown::Unfinished {
            stops: reasons.clone(),
        },
        decide::gap::Gap::Dispatched { via } => Unknown::Dispatched {
            trait_name: via.clone(),
        },
        other => Unknown::Uncovered {
            why: other.to_string(),
        },
    }
}

/// The evidence of a row for a module that re-exports the definition: where
/// it reaches the package's users, not a caller of it.
pub(super) const REEXPORT: &str = "reexport";

/// A caller row as the resolver gave it, for a line no fragment owns: a
/// script the fragmenter did not cut, or a file past its fragment cap.
pub(super) fn cell_row(u: &decide::verdict::Use) -> Caller {
    let reach = reach_of(u);
    let reexport = u.relation == diffctx_resolve::resolve::Relation::Reexports;
    let within = match &u.scope {
        diffctx_facts::facts::Scope::Def(local) => Some(local.clone()),
        _ => None,
    };
    let caller = within.as_ref().map(|local| local.name.as_str().to_string());
    Caller {
        within,
        reason: reach.reason().filter(|_| !reexport),
        kind: reach.stated_kind(&u.file),
        folded_by: reach.fold.map(|(rule, _, _)| rule),
        fold_kind: reach.fold.map(|(_, kind, _)| kind),
        fold_share: reach.fold.map_or(0, |(_, _, share)| share),
        ..Caller::at_line(
            u.file.clone(),
            caller,
            format!("{0}-{0}", u.line),
            if reexport { REEXPORT } else { reach.evidence() },
            if reexport {
                "reexports"
            } else {
                reach.relation
            },
            u.line,
        )
    }
}

/// A holder nothing uses itself is changed through what it holds: the use of a
/// held definition, or what the search could not tell of it, is that row's to
/// say. "Nothing calls it" of the holder would be an all-clear the held row's
/// gap contradicts.
pub(super) fn held_has_users(cell: &decide::cell::Cell, holder: &decide::changed::Changed) -> bool {
    cell.changed().iter().any(|held| {
        holder.holds(held) && {
            let verdict = cell.verdict(held);
            !verdict.uses.is_empty() || !verdict.complete() || !verdict.unused()
        }
    })
}

/// A definition the answer reports without a fragment to hang the roles
/// and test links on: its callers are the resolver's rows as they stand.
pub(super) fn cell_symbol(
    def: &decide::changed::Changed,
    verdict: &decide::verdict::Verdict,
    kind: String,
) -> ChangedSymbol {
    let callers: Vec<Caller> = verdict
        .resolved()
        .chain(verdict.candidates())
        .map(cell_row)
        .collect();
    let users = |resolved: bool| -> Vec<String> {
        verdict
            .also_changed()
            .filter(|u| (u.standing == Standing::Resolved) == resolved)
            .map(|u| place(&u.file, None, &u.line.to_string()))
            .collect()
    };
    let (also_changed, possibly_also_changed) = (users(true), users(false));
    ChangedSymbol {
        callers: {
            let mut merged = merge_by_symbol(callers);
            merged.sort_by_key(|c| relation_rank(c.relation));
            merged
        },
        unresolved: verdict.gap().map(|gap| cell_unknown(&gap, &def.name)),
        missed_in: unaccounted_rows(verdict, &def.file),
        also_changed,
        possibly_also_changed,
        change: Some(crate::answer::v2::Change::of(def)),
        new_parameters: def.new_parameters(),
        ..ChangedSymbol::bare(
            def.file.clone(),
            def.symbol(),
            format!("{}-{}", def.lines.0, def.lines.1),
            kind,
            decide::coverage::Coverages::of_verdict(verdict),
        )
    }
}
