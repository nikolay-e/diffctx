use crate::types::{Fragment, FragmentId, FragmentKind};

/// The fragment itself when it is a named container, else the smallest
/// named container in the same file that encloses it.
pub fn named_container_of<'a>(core: &'a Fragment, all: &'a [Fragment]) -> Option<&'a Fragment> {
    let is_named = |f: &Fragment| f.kind.is_definition_kind() && f.symbol_name.is_some();
    if is_named(core) {
        return Some(core);
    }
    all.iter()
        .filter(|f| {
            f.path() == core.path()
                && is_named(f)
                && f.id.start_line <= core.id.start_line
                && f.id.end_line >= core.id.end_line
                && f.id != core.id
        })
        .min_by_key(|f| f.line_count())
        // A module-level `export const f = (…) => …` is a definition in all
        // but kind: nothing encloses it, and its importers are its callers
        // (#324). Inside a function a variable is the function's business.
        .or_else(|| {
            (core.kind == FragmentKind::Variable && core.symbol_name.is_some()).then_some(core)
        })
}

/// Of fragments nested in one another (a method and its impl, a function
/// and its enclosing class), the innermost is the one that says where.
pub fn innermost<'a>(mut frags: Vec<&'a Fragment>) -> Vec<&'a Fragment> {
    frags.sort_by(|a, b| a.id.cmp(&b.id));
    let keep: Vec<bool> = frags
        .iter()
        .map(|outer| {
            !frags.iter().any(|inner| {
                inner.id != outer.id
                    && inner.path() == outer.path()
                    && inner.id.start_line >= outer.id.start_line
                    && inner.id.end_line <= outer.id.end_line
            })
        })
        .collect();
    frags
        .into_iter()
        .zip(keep)
        .filter_map(|(f, k)| k.then_some(f))
        .collect()
}

pub fn within(id: &FragmentId, outer: &Fragment) -> bool {
    id.path.as_ref() == outer.path()
        && id.start_line >= outer.start_line()
        && id.end_line <= outer.end_line()
}
