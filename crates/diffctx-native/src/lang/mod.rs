//! The definitions a run lays over the fragments (the overlay) with the names a file
//! defines that a pattern over its lines does not see.

mod bound;
mod shell;
mod sql;

use std::path::Path;
use std::sync::Arc;

use rustc_hash::FxHashMap;

use crate::bindings::{self, Lang};
use crate::facts::Declaration;
use crate::source::Source;
use crate::types::{Fragment, FragmentId, FragmentKind};

/// A file's declarations as `(name, lines, kind)`, read from its path and
/// text: a page's code is its inline scripts.
type Declarations = fn(&str, &str) -> Vec<(Option<String>, (u32, u32), FragmentKind)>;

fn declarations_of(path: &str) -> Option<Declarations> {
    match bindings::lang_of(path) {
        Some(Lang::Python) => Some(|_, text| python_assignments(text)),
        Some(Lang::Js) => Some(bound::declarations),
        None if sql::is_sql(path) => Some(|_, text| sql::declarations(text)),
        None => None,
    }
}

/// Module- and class-level assignments of a Python file, which impact and
/// `--symbol` lay over the fragments as definitions (#420, #488): the facts
/// layer reports them as definitions, and the fragments they are anchored on
/// have to exist.
fn python_assignments(text: &str) -> Vec<(Option<String>, (u32, u32), FragmentKind)> {
    crate::facts::python::facts(text).map_or_else(Vec::new, |facts| {
        facts
            .declarations
            .iter()
            .filter(|d| d.kind == crate::facts::DeclKind::Assignment)
            .map(|d| (Some(d.name.clone()), d.lines, FragmentKind::Property))
            .collect()
    })
}

/// Definitions a model reads that the fragmenter does not cut: Python
/// module- and class-level assignments (#420, #488), JavaScript properties
/// of the global object or of the module's exports (#426), TypeScript
/// interface members and class fields (#488), SQL functions, procedures
/// and views (#459). Pack keeps its fragments as they are, sized for a
/// reader's budget, and an impact run lays these over them; a fragment
/// spanning exactly the definition becomes it, and keeps its kind when it
/// already is a definition's (an arrow assigned to `self.serveShell`).
pub fn overlay_definitions(fragments: &mut Vec<Fragment>, source: &Source) {
    let mut paths: Vec<Arc<str>> = fragments
        .iter()
        .filter(|f| declarations_of(f.path()).is_some())
        .map(|f| Arc::clone(&f.id.path))
        .collect();
    paths.sort();
    paths.dedup();
    let mut at: FxHashMap<FragmentId, usize> = fragments
        .iter()
        .enumerate()
        .map(|(i, f)| (f.id.clone(), i))
        .collect();
    for path in paths {
        let (Some(read), Some(text)) = (
            declarations_of(&path),
            source.read_to_string(Path::new(path.as_ref())),
        ) else {
            continue;
        };
        let lines: Vec<&str> = text.split('\n').collect();
        for (name, (start, end), kind) in read(&path, &text) {
            let id = FragmentId::new(Arc::clone(&path), start, end);
            if let Some(&i) = at.get(&id) {
                let same = &mut fragments[i];
                if same.symbol_name.is_none() && name.is_some() {
                    if !same.kind.is_definition_kind() {
                        same.kind = kind;
                    }
                    same.symbol_name = name;
                }
                continue;
            }
            let Some(content) = lines
                .get(start as usize - 1..end as usize)
                .map(|l| l.join("\n"))
            else {
                continue;
            };
            at.insert(id.clone(), fragments.len());
            fragments.push(Fragment {
                id,
                kind,
                identifiers: crate::types::extract_identifiers(
                    &content,
                    crate::config::tokenization::TOKENIZATION.fragment_min_identifier_length,
                ),
                content: Arc::from(content),
                token_count: 0,
                symbol_name: name,
            });
        }
    }
}

/// The names a file defines that a pattern over its lines does not see:
/// shell functions (`fetch() {`), SQL routines and views, a Python module's
/// assignments, a JavaScript module's definitions and the globals it sets
/// (`self.serveShell = …`). Impact looks for the files that spell them.
pub fn declared_names(path: &str, text: &str) -> Vec<String> {
    let reachable = |declared: &[Declaration]| {
        declared
            .iter()
            .filter(|d| {
                d.scope.is_none()
                    && d.class
                        .as_deref()
                        .is_none_or(|c| c == bindings::GLOBAL_OWNER)
            })
            .map(|d| d.name.clone())
            .collect()
    };
    match bindings::lang_of(path) {
        Some(Lang::Python) => {
            crate::facts::python::facts(text).map_or_else(Vec::new, |f| reachable(&f.declarations))
        }
        Some(Lang::Js) => {
            bindings::js_facts(path, text).map_or_else(Vec::new, |f| reachable(&f.declarations))
        }
        None if sql::is_sql(path) => sql::declarations(text)
            .into_iter()
            .filter_map(|(n, _, _)| n)
            .collect(),
        None => shell::function_names(path, text),
    }
}

/// Whether a `--symbol` qualifier names the owner a model states for a
/// definition: a SQL schema (`lib.refresh_book`). `None` when the model
/// states none.
pub fn stated_owner(path: &str, line: u32, owner: &str, source: &Source) -> Option<bool> {
    if !sql::is_sql(path) {
        return None;
    }
    let text = source.read_to_string(Path::new(path))?;
    Some(sql::schema_at(&text, line).as_deref() == Some(&owner.to_lowercase()))
}
