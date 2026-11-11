//! JavaScript and the inline scripts of HTML pages: the definitions the fragmenter does
//! not cut, which an impact run lays over the fragments. The definitions of JavaScript
//! and TypeScript files and of a page's scripts are the facts layer's (`decide.rs`,
//! `facts::lang::html`); this only gives the fragmenter something to anchor them on.

use crate::bindings;
use crate::facts::DeclKind;
use crate::types::FragmentKind;

/// A JavaScript file's or a page's assignments the fragmenter does not cut
/// — a property of the global object or of the module's exports (#426), an
/// interface member, a class field (#488) — and a page's top-level script
/// statements, so a call there is a caller at its own lines rather than
/// the whole script element (#577).
pub(super) fn declarations(
    path: &str,
    text: &str,
) -> Vec<(Option<String>, (u32, u32), FragmentKind)> {
    bindings::js_facts(path, text).map_or_else(Vec::new, |facts| {
        facts
            .declarations
            .iter()
            .filter(|d| d.kind == DeclKind::Assignment)
            .map(|d| (Some(d.name.clone()), d.lines, FragmentKind::Property))
            .chain(
                facts
                    .script_statements
                    .iter()
                    .map(|&lines| (None, lines, FragmentKind::Chunk)),
            )
            .collect()
    })
}
