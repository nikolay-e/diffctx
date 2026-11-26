//! `git grep` over a revision or the working tree: its tracked and unignored
//! untracked files, as every other reader of the working tree lists them.
//! Rows come back raw; which of their files the run may show is the caller's
//! policy, applied where the rows are read.

use std::path::Path;

use regex::escape;
use rustc_hash::FxHashSet;

use crate::snapshot::diff::validate_pathspec;
use crate::snapshot::error::Result;
use crate::snapshot::git::{Exec, git_command, refusal};
use diffctx_base::paths::contains_lexically;

/// One matching line: repo-relative path, line number, text.
pub type Row = (String, u32, String);

/// Needles sent to one `git grep` by [`needle_lines`] and [`word_lines`], and
/// the bytes of them: `-e` arguments past `ARG_MAX` fail the search (#440), and
/// `-F`'s matcher spends 22, 57 and 242 ms a pattern at 50, 200 and 1000 a call.
const NEEDLES_PER_CALL: usize = 50;
const NEEDLE_BYTES_PER_CALL: usize = 32 * 1024;

/// `git grep`'s stdout; exit 1 means "no match", which is an answer here, not
/// a failure.
fn run_grep(root: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = git_command(root);
    cmd.args(args);
    let exec = Exec::current();
    let output = exec.output(cmd)?;
    if output.status.code() == Some(1) && output.stderr.is_empty() {
        return Ok(String::new());
    }
    if !output.status.success() {
        return Err(refusal(args, &output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `git grep -n -z` at `rev` as rows. The working tree is its tracked and
/// unignored untracked files: without `--untracked` a new file's own callers
/// and tests were nowhere, and its definitions read as called by nothing
/// (#625).
fn rows(root: &Path, args: &[&str], rev: Option<&str>) -> Result<Vec<Row>> {
    let prefix = match rev {
        Some(r) => format!("{r}:"),
        None => String::new(),
    };
    let mut args = args.to_vec();
    if rev.is_none() {
        args.insert(1, "--untracked");
    }
    Ok(run_grep(root, &args)?
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\0');
            let path = parts.next()?;
            let path = path.strip_prefix(prefix.as_str()).unwrap_or(path);
            let Ok(no) = parts.next()?.parse::<u32>() else {
                return None;
            };
            Some((path.to_string(), no, parts.next()?.to_string()))
        })
        .collect())
}

/// Every line matching `pattern` at `rev` (the working tree when `None`) in the
/// files `pathspec` names.
///
/// PCRE answers a 3.4 kB alternation in 0.27 s where git's POSIX dialect takes
/// 10.8 s, so a pattern must mean the same in both.
pub fn lines_matching(
    root: &Path,
    pattern: &str,
    rev: Option<&str>,
    pathspec: &[&str],
) -> Result<Vec<Row>> {
    let search = |dialect: &str| {
        let mut args: Vec<&str> = vec!["grep", "-n", "-z", "-I", dialect, "-e", pattern];
        args.extend(rev);
        if !pathspec.is_empty() {
            args.push("--");
            args.extend_from_slice(pathspec);
        }
        rows(root, &args, rev)
    };
    search("-P").or_else(|_| search("-E"))
}

/// The files (repo-relative, inside the repository) tracked or untracked and
/// unignored that hold a line `pattern` selects.
fn files(root: &Path, pattern: &[&str], pathspec: &[String]) -> Result<Vec<String>> {
    let mut args: Vec<&str> = vec!["grep", "--untracked", "-l", "-z", "-I"];
    args.extend_from_slice(pattern);
    if !pathspec.is_empty() {
        validate_pathspec(pathspec)?;
        args.push("--");
        args.extend(pathspec.iter().map(String::as_str));
    }
    Ok(run_grep(root, &args)?
        .split('\0')
        .filter(|p| !p.is_empty() && contains_lexically(Path::new(p)))
        .map(str::to_string)
        .collect())
}

/// Tracked and unignored untracked files holding `word` as a whole word.
pub fn files_with_word(root: &Path, word: &str, pathspec: &[String]) -> Result<Vec<String>> {
    files(root, &["-w", "-F", "-e", word], pathspec)
}

/// The files holding `word` as a whole word in any case.
pub fn files_with_word_any_case(
    root: &Path,
    word: &str,
    pathspec: &[String],
) -> Result<Vec<String>> {
    files(root, &["-w", "-F", "-i", "-e", word], pathspec)
}

/// The files with a line that declares `name`: `def name`, `class name`,
/// `fn name`, `func (r T) name`, `const name =`, `name = function`.
pub fn files_with_declaration(root: &Path, name: &str, pathspec: &[String]) -> Result<Vec<String>> {
    let name = escape(name);
    let pattern = format!(
        r#"((def|class|fn|fun|func|function|struct|interface|trait|enum|type|typealias|module|const|let|var|val|object|define)[[:space:]]+(\([^)]*\)[[:space:]]*)?["']?{name}([^[:alnum:]_$]|$))|((^|[^[:alnum:]_$.]){name}[[:space:]]*[:=][[:space:]]*(function|async|\())"#
    );
    files(root, &["-E", "-e", &pattern], pathspec)
}

/// Lines of the files `globs` select at `rev` that hold one of `words` as a
/// whole word, at most `per_file` per file: where a removed definition's
/// callers are read from. Unbounded, `-F -e call` matched every `callback` in
/// every data file of a monorepo and the run never stopped (#413, #457).
///
/// Searched in chunks, as [`needle_lines`] does: one `-F -w` search over tens
/// of thousands of names ran for hours, and orphaned when its run was killed
/// (fj#211). `more` is asked before each chunk; past the run's deadline the
/// rows found are the answer.
pub fn word_lines(
    root: &Path,
    words: &[String],
    rev: Option<&str>,
    per_file: usize,
    globs: &[String],
    more: &dyn Fn() -> bool,
) -> Result<Vec<Row>> {
    let per_file_arg = per_file.to_string();
    let globs: Vec<&str> = globs.iter().map(String::as_str).collect();
    let mut found = chunked(
        root,
        words,
        rev,
        more,
        &["-w", "-m", per_file_arg.as_str()],
        &globs,
    )?;
    // `-m` bounds each chunk's search: the chunks together may hold more.
    let mut kept_for: Option<String> = None;
    let mut kept = 0;
    found.retain(|(path, _, _)| {
        if kept_for.as_deref() != Some(path.as_str()) {
            kept_for = Some(path.clone());
            kept = 0;
        }
        kept += 1;
        kept <= per_file
    });
    Ok(found)
}

/// The first line of each file that holds `word` as a whole word, in the
/// working tree: where a name nothing defines is spelled.
pub fn word_first_lines(root: &Path, word: &str, pathspec: &[String]) -> Result<Vec<Row>> {
    let mut args: Vec<&str> = vec!["grep", "-n", "-z", "-I", "-F", "-w", "-m", "1", "-e", word];
    if !pathspec.is_empty() {
        validate_pathspec(pathspec)?;
        args.push("--");
        args.extend(pathspec.iter().map(String::as_str));
    }
    rows(root, &args, None)
}

/// One run's fixed-string searches of one revision (or the working tree) as a
/// single body of rows: every needle searched so far is `covered`, and the rows
/// are every line holding any of them. A later question about covered needles
/// is answered from the rows; the uncovered ones go to one `git grep` together.
#[derive(Debug, Default)]
pub(crate) struct Searches {
    covered: FxHashSet<String>,
    rows: Vec<Row>,
}

/// Every line holding one of `needles` (fixed strings) at `rev`, or in the
/// working tree when `rev` is `None`, sorted by path then line. Within a run,
/// one body of rows serves every question: a needle is searched once.
pub fn needle_lines(
    root: &Path,
    needles: &[String],
    rev: Option<&str>,
    more: &dyn Fn() -> bool,
) -> Result<Vec<Row>> {
    let Some(shared) = Exec::current().searches(root, rev) else {
        return chunked(root, needles, rev, more, &[], &[]);
    };
    let mut searches = shared.lock().unwrap();
    let wanted: FxHashSet<&str> = needles
        .iter()
        .map(String::as_str)
        .filter(|n| !n.is_empty())
        .collect();
    if wanted.iter().any(|n| !searches.covered.contains(*n)) {
        let mut missing: Vec<String> = wanted
            .iter()
            .map(|n| (*n).to_string())
            .filter(|n| !searches.covered.contains(n))
            .collect();
        missing.sort_unstable();
        let found = chunked(root, &missing, rev, more, &[], &[])?;
        searches.rows.extend(found);
        searches.rows.sort_unstable();
        searches.rows.dedup();
        // A search the deadline cut short covers nothing: its rows are what
        // was found, and nobody else is told they are the answer.
        if more() {
            searches.covered.extend(missing);
        }
    }
    // Sizes alone would take covered {a, b} for wanted {c, d} after a cut search.
    let everything = searches.covered.len() == wanted.len()
        && wanted.iter().all(|n| searches.covered.contains(*n));
    let automaton = (!everything).then(|| aho_corasick::AhoCorasick::new(&wanted));
    let selected = |text: &str| match &automaton {
        Some(Ok(found)) => found.is_match(text),
        _ => wanted.iter().any(|n| text.contains(n)),
    };
    Ok(searches
        .rows
        .iter()
        .filter(|(_, _, text)| everything || selected(text))
        .cloned()
        .collect())
}

fn chunked(
    root: &Path,
    needles: &[String],
    rev: Option<&str>,
    more: &dyn Fn() -> bool,
    flags: &[&str],
    pathspec: &[&str],
) -> Result<Vec<Row>> {
    let mut needles: Vec<&str> = needles.iter().map(String::as_str).collect();
    needles.retain(|n| !n.is_empty());
    let mut found = Vec::new();
    for words in chunks(&needles) {
        if !more() {
            break;
        }
        // 50 needles as one `-P` alternation take 0.13 s on a monorepo, 102 s as `-F -e`.
        // `-w` tries only the first alternative matching at a place: boundaries go in the pattern.
        let escaped: Vec<_> = words.iter().map(|w| escape(w)).collect();
        let mut pattern = escaped.join("|");
        if flags.contains(&"-w") {
            pattern = format!(r"(?<!\w)(?:{pattern})(?!\w)");
        }
        let search = |pcre: bool| {
            let mut args = vec!["grep", "-n", "-z", "-I", if pcre { "-P" } else { "-F" }];
            args.extend(flags.iter().filter(|f| !pcre || **f != "-w"));
            let patterns: &[&str] = if pcre { &[pattern.as_str()] } else { words };
            args.extend(patterns.iter().flat_map(|p| ["-e", *p]));
            args.extend(rev);
            if !pathspec.is_empty() {
                args.push("--");
                args.extend(pathspec);
            }
            rows(root, &args, rev)
        };
        found.extend(search(true).or_else(|_| search(false))?);
    }
    found.sort_unstable();
    found.dedup();
    Ok(found)
}

/// The needles in runs one search takes. They go as `-e` arguments: a pattern
/// file would outlive a run that is killed (fj#211).
fn chunks<'a>(needles: &'a [&'a str]) -> impl Iterator<Item = &'a [&'a str]> {
    let mut rest = needles;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let mut bytes = 0;
        let taken = rest
            .iter()
            .take(NEEDLES_PER_CALL)
            .take_while(|needle| {
                bytes += needle.len();
                bytes <= NEEDLE_BYTES_PER_CALL
            })
            .count()
            .max(1);
        let (chunk, tail) = rest.split_at(taken);
        rest = tail;
        Some(chunk)
    })
}
