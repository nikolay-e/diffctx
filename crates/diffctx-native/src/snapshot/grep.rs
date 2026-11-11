//! `git grep` over a revision or the working tree: its tracked and unignored
//! untracked files, as every other reader of the working tree lists them.
//! Rows come back raw; which of their files the run may show is the caller's
//! policy, applied where the rows are read.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use regex::escape;

use crate::snapshot::diff::validate_pathspec;
use crate::snapshot::error::Result;
use crate::snapshot::git::{Exec, git_command};
use crate::snapshot::types::contains_lexically;

/// One matching line: repo-relative path, line number, text.
pub type Row = (String, u32, String);

// PID alone is not unique within a process: the MCP server runs each tool body
// on its own worker thread, so two overlapping pipelines can write a temp file
// under the same PID. The counter makes every call's temp path unique
// regardless of thread interleaving.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Needles sent to one `git grep` by [`needle_lines`].
const NEEDLES_PER_CALL: usize = 50;

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

/// Every line matching the extended regex `pattern` at `rev` (the working tree
/// when `None`) in the files `pathspec` names.
pub fn lines_matching(
    root: &Path,
    pattern: &str,
    rev: Option<&str>,
    pathspec: &[&str],
) -> Result<Vec<Row>> {
    let mut args: Vec<&str> = vec!["grep", "-n", "-z", "-I", "-E", "-e", pattern];
    args.extend(rev);
    if !pathspec.is_empty() {
        args.push("--");
        args.extend_from_slice(pathspec);
    }
    rows(root, &args, rev)
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
pub fn word_lines(
    root: &Path,
    words: &[String],
    rev: Option<&str>,
    per_file: usize,
    globs: &[String],
) -> Result<Vec<Row>> {
    let per_file = per_file.to_string();
    let patterns = Patterns::new(words);
    let mut args: Vec<&str> = vec!["grep", "-n", "-z", "-I", "-F", "-w", "-m", &per_file];
    args.extend(patterns.args.iter().map(String::as_str));
    args.extend(rev);
    args.push("--");
    args.extend(globs.iter().map(String::as_str));
    rows(root, &args, rev)
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

/// Every line holding one of `needles` (fixed strings) at `rev`, or in the
/// working tree when `rev` is `None`, sorted by path then line.
///
/// One `-F` search over a thousand needles costs more than twenty over fifty:
/// git's matcher is superlinear in the needle count (540 CPU seconds for 1150
/// needles on a 387-file range). Chunked, a line that holds needles of two
/// chunks comes back twice and is kept once. `more` is asked before each
/// chunk: past the run's deadline the rows found are the answer, and the run
/// says it is partial.
pub fn needle_lines(
    root: &Path,
    needles: &[String],
    rev: Option<&str>,
    more: &dyn Fn() -> bool,
) -> Result<Vec<Row>> {
    let mut found = Vec::new();
    for chunk in needles.chunks(NEEDLES_PER_CALL) {
        if chunk.iter().all(String::is_empty) {
            continue;
        }
        if !more() {
            break;
        }
        let patterns = Patterns::new(chunk);
        let mut args: Vec<&str> = vec!["grep", "-n", "-z", "-I", "-F"];
        args.extend(patterns.args.iter().map(String::as_str));
        args.extend(rev);
        found.extend(rows(root, &args, rev)?);
    }
    found.sort_unstable();
    found.dedup();
    Ok(found)
}

/// `-f FILE` naming the patterns one per line, or `-e` per pattern when the
/// file cannot be written: tens of thousands of `-e` arguments overflow
/// ARG_MAX and the whole search fails (#440). The file goes with the value.
struct Patterns {
    file: Option<PathBuf>,
    args: Vec<String>,
}

impl Patterns {
    fn new(words: &[String]) -> Self {
        // An empty line would match every line.
        let listed: Vec<&str> = words
            .iter()
            .map(String::as_str)
            .filter(|w| !w.is_empty())
            .collect();
        match write_private_temp_file(&listed.join("\n")) {
            Some(file) => Self {
                args: vec!["-f".to_string(), file.to_string_lossy().into_owned()],
                file: Some(file),
            },
            None => Self {
                file: None,
                args: listed
                    .iter()
                    .flat_map(|w| ["-e".to_string(), (*w).to_string()])
                    .collect(),
            },
        }
    }
}

impl Drop for Patterns {
    fn drop(&mut self) {
        if let Some(file) = &self.file {
            let _ = std::fs::remove_file(file);
        }
    }
}

/// Writes `content` to a fresh file in the system temp directory and returns
/// its path, or `None` if no such file could be created.
///
/// The name used to be written with `fs::write`, which follows symlinks and
/// truncates the target. Both the pid and the counter are guessable, so on a
/// shared machine anyone able to write to the temp directory could pre-plant
/// that name as a symlink and have this overwrite the file it points at, with
/// repository-controlled content. `create_new` is `O_CREAT | O_EXCL`: it
/// refuses an existing path, symlink included, so a planted name costs at most
/// a retry; and if every attempt loses, the caller degrades to one `-e` per
/// pattern.
fn write_private_temp_file(content: &str) -> Option<PathBuf> {
    let dir = std::env::temp_dir();
    for _ in 0..8 {
        let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.subsec_nanos(),
            Err(_) => 0,
        };
        let path = dir.join(format!(
            "diffctx-ignore-{}-{unique}-{nanos}.tmp",
            std::process::id()
        ));
        match create_new_private_file(&path) {
            Ok(mut file) => {
                return match file
                    .write_all(content.as_bytes())
                    .and_then(|()| file.flush())
                {
                    Ok(()) => Some(path),
                    Err(_) => {
                        let _ = std::fs::remove_file(&path);
                        None
                    }
                };
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }
    }
    None
}

/// `O_CREAT | O_EXCL` (`create_new`), which refuses any existing path (a
/// planted symlink included) instead of following it.
fn create_new_private_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test reads back the file it wrote"
)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// The `core.excludesFile` handed to `git check-ignore` used to be written
    /// with `fs::write` under a guessable name in the shared temp directory,
    /// which follows a symlink and truncates its target. Anything the pipeline
    /// creates there must refuse a path that already exists.
    #[test]
    fn temp_file_creation_refuses_a_pre_planted_path() {
        let tmp = TempDir::new().expect("tempdir");
        let victim = tmp.path().join("victim.txt");
        std::fs::write(&victim, "precious\n").expect("write victim");

        let planted = tmp.path().join("planted.tmp");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&victim, &planted).expect("symlink");
        #[cfg(not(unix))]
        std::fs::write(&planted, "").expect("placeholder");

        let err = create_new_private_file(&planted).expect_err("must refuse an existing path");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read_to_string(&victim).expect("victim survives"),
            "precious\n",
            "the symlink target was written through"
        );
    }

    #[test]
    fn temp_excludes_file_is_written_and_readable() {
        let path = write_private_temp_file("*.log\n!keep.log").expect("temp file");
        let content = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(content, "*.log\n!keep.log");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "temp excludes file must not be world-readable");
        }
        let _ = std::fs::remove_file(&path);
    }
}
