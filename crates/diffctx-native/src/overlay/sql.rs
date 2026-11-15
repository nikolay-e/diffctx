//! SQL, for the fragments of a SQL file: its routines and views are definitions
//! (`facts::lang::sql` reads them), and the code between them is a chunk of its
//! own, which a chunk spanning a definition would otherwise hide. The calls and
//! reads are the facts layer's (`resolve::sql`).

use std::path::Path;

use crate::facts::lang::sql::defined;
use crate::types::FragmentKind;

pub(super) fn is_sql(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("sql"))
}

/// The definitions of `text` as `(name, lines, kind)`, for the fragments
/// an impact run lays over the text chunks of a SQL file, and the code
/// between them: a call there is a caller of its own.
pub(super) fn declarations(text: &str) -> Vec<(Option<String>, (u32, u32), FragmentKind)> {
    let (definitions, between) = defined(text);
    let between = between
        .into_iter()
        .map(|lines| (None, lines, FragmentKind::Chunk));
    definitions
        .into_iter()
        .map(|d| {
            let kind = if d.routine {
                FragmentKind::Function
            } else {
                FragmentKind::Definition
            };
            (Some(d.spelled), d.lines, kind)
        })
        .chain(between)
        .collect()
}

/// The schema a definition starting on `line` names, `None` when it names
/// none.
pub(super) fn schema_at(text: &str, line: u32) -> Option<String> {
    defined(text)
        .0
        .into_iter()
        .find(|d| d.lines.0 == line)
        .and_then(|d| d.schema)
}
