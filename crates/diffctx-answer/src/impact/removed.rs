//! What a range removed or left dangling: removed definitions, dead modules and stale references.

use std::path::Path;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::decide::cells::Cells;
use crate::decide::{self};

use crate::run_provenance::state::ScoredState;

use diffctx_base::languages::runtime_of;

use diffctx_base::types::{Fragment, FragmentId};

use diffctx_facts::parsers::code_text;

use super::mentions::{is_import_line, reads_imports};

use super::MAX_REMOVED_HITS_PER_FILE;
use super::MAX_REMOVED_NAME_HITS;
use super::MAX_REMOVED_NAMES_TRACED;
use super::callers::is_code_file;
use super::changes::{is_public, lines_of, rel};
use super::dispatch::merge_by_symbol;
use super::output::{Caller, ChangedSymbol, Contract, DeletedModule, StaleReference, Unknown};

/// Definitions the change removed and nothing at the head defines again,
/// with the lines outside the diff that still name them (#367). They have
/// no fragment at the head, so the callers graph never reaches them: an
/// optional call (`engine.setGain?.()`) silently becomes a no-op.
/// Kotlin declarations the range removed: in the base revision of a changed
/// file and gone from it at the head, matched by owner, name, kind and
/// receiver. What still uses one is found by the binding model, at the head.
/// A read the answer depends on, or nothing with the run marked: a failed
/// `git grep` read as no match turned callers and stale references into a
/// clean zero with exit 0.
pub(super) fn read_or_note<T: Default>(
    state: &ScoredState,
    read: diffctx_snapshot::snapshot::Result<T>,
) -> T {
    read.unwrap_or_else(|e| {
        tracing::warn!("impact: a git read failed ({e}); its answer is unknown");
        state
            .run
            .note(diffctx_snapshot::resource::LimitReason::ReadFailed);
        T::default()
    })
}

/// The named definitions of each file in view, by repo-relative path: what
/// holds a line that names a removed definition. Looked up per line, the
/// path of every fragment was rebuilt for every grep hit (#424).
fn definition_holders(state: &ScoredState) -> FxHashMap<String, Vec<&Fragment>> {
    let mut holders: FxHashMap<String, Vec<&Fragment>> = FxHashMap::default();
    for f in state
        .all_fragments
        .iter()
        .filter(|f| f.kind.is_definition_kind() && f.symbol_name.is_some())
    {
        holders.entry(rel(state, f.path())).or_default().push(f);
    }
    holders
}

pub(super) fn removed_definitions(
    state: &ScoredState,
    defined: &FxHashMap<&str, FxHashSet<String>>,
    head: Option<&str>,
    in_a_hunk: &dyn Fn(&FragmentId) -> bool,
    read_off_facts: &dyn Fn(&str) -> bool,
) -> Vec<(ChangedSymbol, Option<Contract>)> {
    // Read off the grammar, the file at the base against the file at the
    // head: a removed definition is one the base declared and the head does
    // not. Read off removed lines by regex, a match arm became a removed
    // member named `Err`, a call statement a removed API, and an indented
    // line of a free function a member of the nearest class above it (#515).
    let old_path_of: FxHashMap<&str, &str> = state
        .renamed_files
        .iter()
        .map(|(old, new)| (new.as_str(), old.as_str()))
        .collect();
    let mut seen: FxHashSet<&str> = FxHashSet::default();
    // (name, path, its path and lines at the base, exported, its class or
    // interface)
    let mut removed: Vec<(String, String, (String, String), bool, Option<String>)> = Vec::new();
    for h in &state.hunk_texts {
        // The facts answer for the files of the cells' languages.
        if h.removed.is_empty()
            || !is_code_file(&h.path)
            || read_off_facts(&h.path)
            || !seen.insert(h.path.as_str())
        {
            continue;
        }
        let old_path = old_path_of.get(h.path.as_str()).copied().unwrap_or(&h.path);
        let before = read_or_note(
            state,
            state
                .run
                .source()
                .base()
                .read_lossy(&state.root_dir.join(old_path))
                // A file with no text at the base is new there.
                .map(|text| text.into_iter().collect::<String>()),
        );
        // A read that failed is noted on the run by the source itself.
        let after = state
            .run
            .source()
            .read_to_string(&state.root_dir.join(&h.path))
            .into_iter()
            .collect::<String>();
        let (Some(at_base), Some(at_head)) = (
            diffctx_facts::parsers::declarations(old_path, &before),
            diffctx_facts::parsers::declarations(&h.path, &after),
        ) else {
            // Declarations the grammar could not read in time (a parse past
            // its budget on a large file) are definitions nobody can name as
            // removed: the answer says so, never "nothing removed".
            if diffctx_facts::parsers::has_grammar(old_path) {
                state.run.note_file(
                    diffctx_snapshot::resource::LimitReason::ReadFailed,
                    old_path.to_string(),
                );
            }
            continue;
        };
        let still: FxHashSet<&str> = at_head.iter().map(|d| d.name.as_str()).collect();
        let public = |d: &diffctx_facts::parsers::Declaration| {
            let head = d.head.trim_start();
            is_public(old_path, Some(&d.name), head)
                || d.container.is_some_and(|c| {
                    !head.starts_with("private")
                        && !head.starts_with("protected")
                        && !head.starts_with('#')
                        && is_public(
                            old_path,
                            Some(&at_base[c].name),
                            at_base[c].head.trim_start(),
                        )
                })
        };
        for d in &at_base {
            if d.name.len() < 3
                || still.contains(d.name.as_str())
                || removed.iter().any(|(n, _, _, _, _)| *n == d.name)
            {
                continue;
            }
            removed.push((
                d.name.clone(),
                h.path.clone(),
                (
                    old_path.to_string(),
                    format!("{}-{}", d.start_line, d.end_line),
                ),
                public(d),
                d.container.map(|c| at_base[c].name.clone()),
            ));
        }
    }
    // One grep for each chunk of removed names, its lines handed to the names
    // they spell as a word: a deleted 1000-function module ran 1000 greps and
    // outlived the deadline.
    let names: Vec<String> = removed
        .iter()
        .take(MAX_REMOVED_NAMES_TRACED)
        .map(|(n, _, _, _, _)| n.clone())
        .collect();
    if removed.len() > names.len() {
        state
            .run
            .note(diffctx_snapshot::resource::LimitReason::CandidateLimit);
    }
    let wanted: FxHashSet<&str> = names.iter().map(String::as_str).collect();
    let mut hits_of: FxHashMap<&str, Vec<(String, u32, String)>> = FxHashMap::default();
    let grepped = read_or_note(
        state,
        diffctx_snapshot::grep::grep_code_words(
            &state.root_dir,
            &state.run.source(),
            &names,
            head,
            MAX_REMOVED_HITS_PER_FILE + 1,
        ),
    );
    state.run.note_withheld(grepped.withheld);
    // The line past the cap says a file held more than it kept, whichever names
    // they spell.
    let mut in_file: (&str, usize) = ("", 0);
    let mut cut = false;
    let rows: Vec<_> = grepped
        .rows
        .iter()
        .filter(|(path, _, _)| {
            in_file = if in_file.0 == path.as_str() {
                (in_file.0, in_file.1 + 1)
            } else {
                (path.as_str(), 1)
            };
            cut |= in_file.1 > MAX_REMOVED_HITS_PER_FILE;
            in_file.1 <= MAX_REMOVED_HITS_PER_FILE
        })
        .collect();
    if cut {
        state
            .run
            .note(diffctx_snapshot::resource::LimitReason::CandidateLimit);
    }
    for (path, line, text) in rows {
        let spelled: FxHashSet<&str> = text
            .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
            .filter_map(|w| wanted.get(w).copied())
            .collect();
        for name in spelled {
            hits_of
                .entry(name)
                .or_default()
                .push((path.clone(), *line, text.clone()));
        }
    }
    let holders = definition_holders(state);
    let mut out = Vec::new();
    for (at, (name, path_removed, (path_at_base, lines), exported, _)) in
        removed.into_iter().enumerate()
    {
        if !state.run.check() {
            break;
        }
        let hits: Vec<_> = hits_of
            .remove(name.as_str())
            .into_iter()
            .flatten()
            .collect();
        let mut callers: Vec<Caller> = Vec::new();
        // A name on this many lines is a common word, not a reference to the
        // removed definition: tracing it by name lists noise and, on a
        // monorepo, never finishes.
        let unresolved = if at >= MAX_REMOVED_NAMES_TRACED {
            Some(Unknown::Unfinished {
                stops: vec![diffctx_snapshot::resource::LimitReason::CandidateLimit],
            })
        } else {
            (hits.len() > MAX_REMOVED_NAME_HITS).then(|| Unknown::CommonWord {
                name: name.to_string(),
                lines: hits.len(),
            })
        };
        let hits = if unresolved.is_some() {
            Vec::new()
        } else {
            hits
        };
        // Defined again elsewhere: only the lines still bound to the old
        // module are broken; the rest already reach the new definition.
        // Without a binding model only a line qualified by the new module
        // (`new::tax_rate`) is known to reach it; dropping every other line
        // read a still-broken `old::tax_rate()` as fixed.
        let moved_to = defined.get(name.as_str()).filter(|at| !at.is_empty());
        let qualified_by = |text: &str, file: &str| {
            let stem = Path::new(file)
                .file_stem()
                .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
            text.contains(&format!("{stem}::{name}")) || text.contains(&format!("{stem}.{name}"))
        };
        for (path, line, text) in hits {
            if !is_code_file(&path) {
                continue;
            }
            // A caller is code that can still reach the removed definition:
            // through an import the binding model resolves to the file it
            // left, else at least from the same runtime. By name alone,
            // `subprocess.run`, another file's own `run`, a shell `die` and a
            // TypeScript `test.describe` were callers (#470, #471, #458).
            let modelled = reads_imports(&path) && reads_imports(&path_removed);
            // No model binds a name across files: of the files a removed
            // definition's language reads, only its own lines are known readers,
            // and a row no model checked is a word match.
            if modelled {
                if path != path_removed {
                    continue;
                }
            } else if runtime_of(&path) != runtime_of(&path_removed)
                // A file that declares the name itself calls its own.
                || holders.get(&path).is_some_and(|fs| {
                    fs.iter().any(|f| f.symbol_name.as_deref() == Some(name.as_str()))
                })
            {
                continue;
            }
            if let Some(at) = moved_to {
                let reaches_new = if reads_imports(&path) {
                    true
                } else {
                    at.iter().any(|f| qualified_by(&text, f)) && !qualified_by(&text, &path_removed)
                };
                if reaches_new {
                    continue;
                }
            }
            let code = code_text(&path, &text);
            // The import is where the name was taken, not a use (#450).
            if is_import_line(&code) {
                continue;
            }
            let bytes = code.as_bytes();
            let Some(at) = code
                .match_indices(name.as_str())
                .map(|(at, _)| at)
                .find(|&at| {
                    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
                    !at.checked_sub(1).is_some_and(|i| word(bytes[i]))
                        && !bytes.get(at + name.len()).copied().is_some_and(word)
                })
            else {
                continue;
            };
            let holder = holders
                .get(&path)
                .into_iter()
                .flatten()
                .filter(|f| f.start_line() <= line && f.end_line() >= line)
                .min_by_key(|f| f.line_count())
                .copied();
            // The line declaring the name is where it went (a move), never a
            // caller of the removed definition; a method of the same name
            // that delegates to it still is one.
            if holder.is_some_and(|f| {
                (in_a_hunk(&f.id) && f.line_count() <= 1)
                    || (f.symbol_name.as_deref() == Some(name.as_str()) && f.start_line() == line)
            }) {
                continue;
            }
            let next = code[at + name.len()..].trim_start();
            callers.push(Caller::at_line(
                path.clone(),
                holder.and_then(|f| f.symbol_name.clone()),
                holder.map_or_else(|| format!("{line}-{line}"), |f| lines_of(&f.id)),
                "name",
                if next.starts_with('(') || next.starts_with("?.(") {
                    "call"
                } else {
                    "reference"
                },
                line,
            ));
        }
        let callers = merge_by_symbol(callers);
        let contract = exported.then(|| Contract {
            path: path_at_base.clone(),
            symbol: Some(name.clone()),
            kind: "removed_api",
        });
        out.push((
            ChangedSymbol {
                callers,
                unresolved,
                ..ChangedSymbol::bare(
                    path_at_base,
                    Some(name),
                    lines,
                    "removed",
                    decide::coverage::Coverages::partial(decide::gap::Gap::NotTraced),
                )
            },
            contract,
        ));
    }
    out
}

pub(super) const PROJECT_MANIFESTS: &[&str] = &[
    "package.json",
    "pyproject.toml",
    "setup.py",
    "Cargo.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "composer.json",
    "Gemfile",
    "mix.exs",
    "deno.json",
];

pub(super) const MAX_STALE_REFERENCES: usize = 24;

pub(super) const MAX_STALE_PER_TARGET: usize = 8;

/// What names a changed file is context, not breakage: fewer lines than
/// what still names a deleted one.
pub(super) const MAX_NAMED_BY: usize = 12;

pub(super) const MAX_NAMED_BY_PER_TARGET: usize = 3;

/// The modules the range deletes, each answered once instead of definition by
/// definition, and the changed list without the definitions it folded. A module's
/// importers are the head files the base bound to it (the cell's facts) and the
/// files that still name its path. A definition keeps its row when something that
/// can reach it uses it: a resolved caller, or a candidate in an importer. A name
/// that other files merely spell is no way to reach a module nothing imports,
/// unless the family's names are global: a shell function runs from wherever its
/// file was loaded, so any candidate keeps it.
/// Only a module of a cell language is read this way: its imports are the facts'.
/// A computed import may reach a module nothing imports, so that claim says "none
/// found" whenever the facts cannot rule one out (`Cells::computed_imports_possible`).
pub(super) fn dead_modules(
    state: &ScoredState,
    cells: &Cells,
    changed: Vec<ChangedSymbol>,
    stale: &[StaleReference],
) -> (Vec<DeletedModule>, Vec<ChangedSymbol>) {
    let renamed: FxHashMap<&str, &str> = state
        .renamed_files
        .iter()
        .map(|(old, new)| (old.as_str(), new.as_str()))
        .collect();
    let mut dead: Vec<DeletedModule> = Vec::new();
    let mut folded: FxHashSet<usize> = FxHashSet::default();
    for file in &state.deleted_files {
        let held: Vec<usize> = (0..changed.len())
            .filter(|&at| changed[at].kind == "removed" && &changed[at].path == file)
            .collect();
        let Some(cell) = cells.of(file).filter(|_| !held.is_empty()) else {
            continue;
        };
        let Some(bound) = cell.importers_of(file, &state.deleted_files) else {
            continue;
        };
        let mut importers: Vec<String> = bound
            .into_iter()
            .map(|path| {
                renamed
                    .get(path.as_str())
                    .map_or(path, |new| (*new).to_string())
            })
            .chain(
                stale
                    .iter()
                    .filter(|r| &r.target == file && r.class.reads())
                    .map(|r| r.path.clone()),
            )
            .collect();
        importers.sort();
        importers.dedup();
        // A definition whose name a file outside what reads it spells, or that was
        // never searched as text, keeps its row: the module's line has no room for it.
        let global = cell.names_are_global();
        let keeps_row = |c: &ChangedSymbol| {
            c.callers
                .iter()
                .any(|k| k.evidence != "candidate" || global || importers.contains(&k.path))
                || !c.missed_in.is_empty()
                || c.coverage.all().gaps().iter().any(|gap| {
                    *gap == decide::gap::Gap::NameUnsearched
                        || (global && matches!(gap, decide::gap::Gap::Candidates { .. }))
                })
        };
        let folding: Vec<usize> = held
            .iter()
            .copied()
            .filter(|&at| !keeps_row(&changed[at]))
            .collect();
        if folding.is_empty() {
            continue;
        }
        let more_importers =
            Some(importers.len().saturating_sub(MAX_NAMED_BY_PER_TARGET)).filter(|n| *n > 0);
        importers.truncate(MAX_NAMED_BY_PER_TARGET);
        dead.push(DeletedModule {
            path: file.clone(),
            definitions: held.len(),
            listed: Some(held.len() - folding.len()).filter(|n| *n > 0),
            importers,
            more_importers,
            dynamic_import: false,
        });
        folded.extend(folding);
    }
    if dead.is_empty() {
        return (dead, changed);
    }
    let dynamic_import = cells.computed_imports_possible();
    for module in &mut dead {
        module.dynamic_import = dynamic_import && module.importers.is_empty();
    }
    let changed = changed
        .into_iter()
        .enumerate()
        .filter(|(at, _)| !folded.contains(at))
        .map(|(_, c)| c)
        .collect();
    (dead, changed)
}
