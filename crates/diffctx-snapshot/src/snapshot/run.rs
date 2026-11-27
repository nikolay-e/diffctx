//! The questions the engine asks git that are not one snapshot's bytes: a
//! command's text, the messages of a range, the blob ids `git add` would
//! store. Each runs under the limits of the run on this thread
//! ([`enter_run`](crate::snapshot::enter_run)), as every child git does.

use std::path::Path;

use crate::snapshot::error::{Result, SnapshotError};
use crate::snapshot::git::{Exec, git_command};
use crate::snapshot::repo::{reject_option_like, rev_oid};

/// `git <args>` in `root`: stdout as text. A non-zero exit is an error naming
/// git's own `fatal:` line.
pub fn run_git(root: &Path, args: &[&str]) -> Result<String> {
    Exec::current().text(root, args)
}

/// `run_git` for a question git answers "no" to by an exit status of its own
/// (`rev-parse --verify --quiet` of an absent ref, `merge-base` of unrelated
/// histories: 1): `None` then. Any other non-zero status is an `Err`.
pub fn run_git_or_none(root: &Path, args: &[&str], no: i32) -> Result<Option<String>> {
    let mut cmd = git_command(root);
    cmd.args(args);
    let out = Exec::current().finish(cmd, args, &[no])?;
    Ok(out
        .status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned()))
}

/// `run_git` for NUL-separated output (`-z`): the non-empty records.
pub fn run_git_z(root: &Path, args: &[&str]) -> Result<Vec<String>> {
    Ok(run_git(root, args)?
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect())
}

/// `base..head` when both ends name commits and differ. `None` is the answer
/// "no commits": `X..X`, or a side that is a tree (the empty tree a window
/// older than the repository starts from, a staged index) or names nothing;
/// a read git could not answer is an `Err`.
fn commit_range(root: &Path, base: &str, head: &str) -> Result<Option<String>> {
    reject_option_like(base)?;
    reject_option_like(head)?;
    if base == head || rev_oid(root, base)?.is_none() || rev_oid(root, head)?.is_none() {
        return Ok(None);
    }
    Ok(Some(format!("{base}..{head}")))
}

/// Every commit message in `base..head`, subject and body, newest first, at
/// most `limit` commits, each body cut at `max_chars`. A range used to be
/// titled by the last commit's subject alone; the body is where a person says
/// why, and on a GitOps branch the last commit is the image updater's. `X..X`
/// holds no commit: a working-tree diff asks it of `HEAD..HEAD`.
pub fn commit_messages(
    root: &Path,
    base: &str,
    head: &str,
    limit: usize,
    max_chars: usize,
) -> Result<Vec<String>> {
    let Some(range) = commit_range(root, base, head)? else {
        return Ok(Vec::new());
    };
    let max = format!("--max-count={limit}");
    // `%B` is the raw message; `%x1e` (record separator) cannot occur in one.
    let out = run_git(root, &["log", "--format=%B%x1e", &max, &range, "--"])?;
    Ok(out
        .split('\x1e')
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(|m| {
            if m.chars().count() <= max_chars {
                m.to_string()
            } else {
                let cut: String = m.chars().take(max_chars).collect();
                format!("{}…", cut.trim_end())
            }
        })
        .collect())
}

/// Commits in `base..head`: the list above stops at `limit`, and a reader told
/// "20 commits" must learn when the range held more.
pub fn commit_count(root: &Path, base: &str, head: &str) -> Result<usize> {
    let Some(range) = commit_range(root, base, head)? else {
        return Ok(0);
    };
    let out = run_git(root, &["rev-list", "--count", &range, "--"])?;
    out.trim().parse().map_err(|_| {
        SnapshotError::Git(format!(
            "git rev-list --count answered '{}', not a number",
            out.trim()
        ))
    })
}

/// The subject and body of one commit, trimmed; empty for a revision that
/// names no commit (a tree, an unborn branch).
pub fn commit_message(root: &Path, rev: &str) -> Result<String> {
    reject_option_like(rev)?;
    if rev_oid(root, rev)?.is_none() {
        return Ok(String::new());
    }
    Ok(run_git(root, &["log", "-1", "--format=%s%n%b", rev])?
        .trim()
        .to_string())
}

/// The blob ids `git add` would store for `paths` (repo-relative), in order:
/// the repository's filters (line endings, LFS) applied, nothing written.
pub fn hash_object_paths(root: &Path, paths: &[String]) -> Result<Vec<String>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut cmd = git_command(root);
    cmd.args(["hash-object", "--stdin-paths"]);
    let input: String = paths.iter().map(|p| format!("{p}\n")).collect();
    let out = Exec::current().feed(cmd, &["hash-object"], input.into_bytes())?;
    let oids: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    if oids.len() != paths.len() {
        return Err(SnapshotError::Git("hash-object: short answer".into()));
    }
    Ok(oids)
}
