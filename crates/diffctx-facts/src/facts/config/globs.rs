//! Fields that select paths by a pattern instead of naming one: nginx's
//! `include snippets/*.conf;`, ruff's `src = ["*/*/*"]`. A [`SourcePointer`]
//! names one place; a glob names a set, and which files of a tree it holds is
//! the caller's question (gh#309, gh#522). The pattern is resolved as a pointer
//! is, from the directory of the file that writes it; one that is computed,
//! rooted or outside the repository selects nothing here.

use crate::facts::Span;
use diffctx_snapshot::snapshot::RunBudget;

use super::pointer::{PointerKind, Resolved, SourcePointer, directory_of};
use super::tree::FieldPath;
use super::{Dialect, Options, read};

/// What a match means to the tool.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Selects {
    /// The files the pattern matches are read whole: an edit of one reaches it.
    Files,
    /// The directories the pattern matches are roots whose entries the tool
    /// names (ruff's first-party packages): only an entry a change adds or
    /// removes moves what the tool sees.
    Roots,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PathGlob {
    pub line: u32,
    /// Repository-relative and normalised; may hold glob characters.
    pub pattern: String,
    pub selects: Selects,
}

/// The structured rows: a file name, the field, what it selects.
const FIELDS: &[(&str, &str, Selects)] = &[
    ("ruff.toml", "src[]", Selects::Roots),
    ("pyproject.toml", "tool.ruff.src[]", Selects::Roots),
];

fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Whether a file at `path` can hold a field a row reads.
pub fn may_select(path: &str) -> bool {
    let name = name_of(path);
    name.ends_with(".conf") || FIELDS.iter().any(|(file, ..)| *file == name)
}

/// The globs the file at `path` writes.
pub fn globs_of(path: &str, text: &str, budget: &RunBudget) -> Vec<PathGlob> {
    let name = name_of(path);
    let mut written: Vec<(u32, String, Selects)> = Vec::new();
    if name.ends_with(".conf") {
        written.extend(
            nginx_includes(text)
                .into_iter()
                .map(|(line, arg)| (line, arg, Selects::Files)),
        );
    }
    for (_, field, selects) in FIELDS.iter().filter(|(file, ..)| *file == name) {
        let reading = read(Dialect::Toml, text.as_bytes(), Options::default(), budget);
        let field = FieldPath::parse(field).expect("a row's field is a field path");
        for document in &reading.documents {
            for hit in document.select(&field) {
                if let Some(arg) = hit.node.as_str() {
                    written.push((hit.node.span.lines.0, arg.to_string(), *selects));
                }
            }
        }
    }
    let base = directory_of(path);
    written
        .into_iter()
        .filter_map(|(line, text, selects)| {
            let pointer = SourcePointer {
                site: Span::default(),
                kind: PointerKind::Path,
                text,
                templated: false,
            };
            match pointer.resolve(base) {
                Resolved::Local(pattern) => Some(PathGlob {
                    line,
                    pattern,
                    selects,
                }),
                _ => None,
            }
        })
        .collect()
}

/// The argument of each `include` directive, by line. nginx reads a relative
/// one from its configuration prefix, which is the directory of the main
/// file in the layout a repository keeps it in.
fn nginx_includes(text: &str) -> Vec<(u32, String)> {
    let mut found = Vec::new();
    for (at, line) in text.lines().enumerate() {
        let code = line.split('#').next().unwrap_or("");
        for statement in code.split([';', '{', '}']) {
            let mut words = statement.split_whitespace();
            if words.next() != Some("include") {
                continue;
            }
            if let (Some(arg), None) = (words.next(), words.next()) {
                let arg = arg.trim_matches(['"', '\'']);
                found.push((u32::try_from(at + 1).unwrap_or(u32::MAX), arg.to_string()));
            }
        }
    }
    found
}
