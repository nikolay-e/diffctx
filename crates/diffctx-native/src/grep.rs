//! What a `git grep` found in the files the run may show. The search itself
//! is the snapshot layer's; here its rows pass the run's withhold policy, at
//! the read: a caller, a stale reference or a declaration in a withheld file
//! is as withheld as the file. Applied per surface instead, each new reader of
//! grep printed `.diffctx/ignore`d paths until someone noticed.

use std::path::{Path, PathBuf};

use rustc_hash::FxHashSet;

use crate::snapshot::{self, Result, Row};
use crate::source::Source;

/// What a search found in the files the run may show, and the files it found
/// and withheld (repo-relative): an answer built on `rows` counts `withheld`,
/// since what those files say was never read (#392).
#[derive(Default)]
pub struct Grepped<T> {
    pub rows: Vec<T>,
    pub withheld: Vec<String>,
}

fn admissible<T>(source: &Source, rows: Vec<T>, rel_of: impl Fn(&T) -> String) -> Grepped<T> {
    let mut rels: Vec<String> = rows.iter().map(&rel_of).collect();
    rels.sort_unstable();
    rels.dedup();
    let withheld = source.withheld(&rels);
    if withheld.is_empty() {
        return Grepped { rows, withheld };
    }
    let hidden: FxHashSet<&str> = withheld.iter().map(String::as_str).collect();
    let rows = rows
        .into_iter()
        .filter(|r| !hidden.contains(rel_of(r).as_str()))
        .collect();
    Grepped { rows, withheld }
}

fn admitted_rows(source: &Source, rows: Vec<Row>) -> Grepped<Row> {
    admissible(source, rows, |(p, _, _)| p.clone())
}

fn admitted_files(source: &Source, repo_root: &Path, rels: Vec<String>) -> Grepped<PathBuf> {
    let found = admissible(source, rels, String::clone);
    Grepped {
        rows: found
            .rows
            .iter()
            .map(|p| crate::paths::repo_join(repo_root, p))
            .collect(),
        withheld: found.withheld,
    }
}

/// Every line matching the extended regex `pattern` at `rev` (the working
/// tree when `None`) in the files `pathspec` names: (path, line, text).
pub fn grep_lines_matching(
    repo_root: &Path,
    source: &Source,
    pattern: &str,
    rev: Option<&str>,
    pathspec: &[&str],
) -> Result<Grepped<Row>> {
    snapshot::lines_matching(repo_root, pattern, rev, pathspec)
        .map(|rows| admitted_rows(source, rows))
}

/// Tracked and unignored untracked files holding `word` as a whole word.
pub fn grep_files_with_word(
    repo_root: &Path,
    source: &Source,
    word: &str,
    pathspec: &[String],
) -> Result<Grepped<PathBuf>> {
    snapshot::files_with_word(repo_root, word, pathspec)
        .map(|rels| admitted_files(source, repo_root, rels))
}

/// The files with a line that declares `name`.
pub fn grep_files_with_declaration(
    repo_root: &Path,
    source: &Source,
    name: &str,
    pathspec: &[String],
) -> Result<Grepped<PathBuf>> {
    snapshot::files_with_declaration(repo_root, name, pathspec)
        .map(|rels| admitted_files(source, repo_root, rels))
}

/// Lines of code files at `rev` that hold one of `words` as a whole word, at
/// most `per_file` per file: where a removed definition's callers are read
/// from.
pub fn grep_code_words(
    repo_root: &Path,
    source: &Source,
    words: &[String],
    rev: Option<&str>,
    per_file: usize,
) -> Result<Grepped<Row>> {
    if words.is_empty() {
        return Ok(Grepped::default());
    }
    let globs: Vec<String> = crate::config::extensions::CODE_EXTENSIONS
        .iter()
        .map(|ext| format!(":(glob,icase)**/*{ext}"))
        .collect();
    snapshot::word_lines(repo_root, words, rev, per_file, &globs)
        .map(|rows| admitted_rows(source, rows))
}

/// The first line of each file that holds `word` as a whole word, in the
/// working tree: where a name nothing defines is spelled.
pub fn grep_word_first_lines(
    repo_root: &Path,
    source: &Source,
    word: &str,
    pathspec: &[String],
) -> Result<Grepped<Row>> {
    snapshot::word_first_lines(repo_root, word, pathspec).map(|rows| admitted_rows(source, rows))
}

/// Every line holding one of `needles` (fixed strings) at `rev`, or in the
/// working tree when `rev` is `None`: (repo-relative path, line, text). Past
/// the run's deadline the rows found are the answer, and the run says it is
/// partial.
pub fn grep_lines(
    repo_root: &Path,
    source: &Source,
    needles: &[String],
    rev: Option<&str>,
) -> Result<Grepped<Row>> {
    snapshot::needle_lines(repo_root, needles, rev, &crate::resource::current_check)
        .map(|rows| admitted_rows(source, rows))
}
