//! A repository as git states it: where it lives, what its refs say, how its
//! history relates. Every answer is read from git; a failed call is an `Err`,
//! never an empty value (AC2).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::snapshot::error::{Result, SnapshotError};
use crate::snapshot::git::{Exec, FATAL_STATUS, git_command};
use crate::snapshot::types::{Cursor, Oid, RefName, ReflogEntry, Remote};

/// A repository.
///
/// The work tree git commands run in, and the common directory
/// that holds the objects and the shared refs. A linked worktree has its own
/// work tree and the main repository's common directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoRoot {
    pub work_tree: PathBuf,
    pub common_dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refs {
    pub head: Option<Oid>,
    pub branches: Vec<(String, Oid)>,
    pub tracking: Vec<(Remote, String, Oid)>,
}

pub fn open(path: &Path) -> Result<RepoRoot> {
    let exec = Exec::ambient();
    let out = exec
        .text(
            path,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
                "--is-bare-repository",
            ],
        )
        .map_err(|e| {
            if refused_without_repository(&e, path) {
                SnapshotError::NotARepo(path.to_path_buf())
            } else {
                e
            }
        })?;
    let mut lines = out.lines();
    let (Some(common), Some(bare)) = (lines.next(), lines.next()) else {
        return Err(SnapshotError::Git(
            "git rev-parse answered without a common directory".into(),
        ));
    };
    let common_dir = canonical(Path::new(common));
    let work_tree = if bare.trim() == "true" {
        common_dir.clone()
    } else {
        let top = exec.text(path, &["rev-parse", "--show-toplevel"])?;
        canonical(Path::new(top.trim()))
    };
    Ok(RepoRoot {
        work_tree,
        common_dir,
    })
}

/// git refused to start in `path` and no repository is there to refuse. Its
/// refusal has one exit status for "no repository" and for every unreadable
/// config, gitfile or ownership, and its words are translated, so the status
/// only says that git refused; the file system says why.
fn refused_without_repository(error: &SnapshotError, path: &Path) -> bool {
    error.git_status() == Some(FATAL_STATUS) && holds_no_repository(path)
}

/// `path` is a directory with no `.git` at or above it and no bare repository
/// at or above it. Anything the file system will not say counts as a possible
/// repository: the answer is then git's refusal, never "no repository".
fn holds_no_repository(path: &Path) -> bool {
    let Ok(start) = dunce::canonicalize(path) else {
        return false;
    };
    let present = |entry: PathBuf| entry.try_exists().unwrap_or(true);
    start.is_dir()
        && !start.ancestors().any(|dir| {
            present(dir.join(".git"))
                || (present(dir.join("HEAD"))
                    && present(dir.join("objects"))
                    && present(dir.join("refs")))
        })
}

/// Git on Windows answers `D:/a/repo`, while every path canonicalised later
/// reads `D:\a\repo\x`; paths are compared as strings downstream.
fn canonical(path: &Path) -> PathBuf {
    dunce::canonicalize(path).unwrap_or_else(|_| path.components().collect())
}

pub fn refs(root: &RepoRoot) -> Result<Refs> {
    let exec = Exec::ambient();
    let work = &root.work_tree;
    let head = rev_to_oid(&exec, work, "HEAD^{commit}")?;
    let listing = exec.text(
        work,
        &[
            "for-each-ref",
            "--format=%(objectname)%00%(refname)%00%(symref)",
            "refs/heads",
            "refs/remotes",
        ],
    )?;
    let remotes: Vec<String> = exec
        .text(work, &["remote"])?
        .lines()
        .map(str::to_string)
        .collect();
    let mut branches = Vec::new();
    let mut tracking = Vec::new();
    for line in listing.lines() {
        let mut fields = line.split('\0');
        let (Some(oid), Some(name), Some(symref)) = (fields.next(), fields.next(), fields.next())
        else {
            return Err(SnapshotError::Git(format!(
                "git for-each-ref answered '{line}'"
            )));
        };
        if !symref.is_empty() {
            continue;
        }
        let oid = Oid::parse(oid)?;
        if let Some(branch) = name.strip_prefix("refs/heads/") {
            branches.push((branch.to_string(), oid));
        } else if let Some(rest) = name.strip_prefix("refs/remotes/") {
            let (remote, branch) = split_tracking(rest, &remotes);
            tracking.push((Remote(remote.to_string()), branch.to_string(), oid));
        }
    }
    Ok(Refs {
        head,
        branches,
        tracking,
    })
}

/// `refs/remotes/<remote>/<branch>` split by the remotes the repository
/// configures, longest name first: remote names may hold slashes, so cutting
/// at the first one would misname both halves. Without a configured match the
/// first slash is the cut.
fn split_tracking<'a>(rest: &'a str, remotes: &[String]) -> (&'a str, &'a str) {
    remotes
        .iter()
        .filter_map(|r| {
            rest.strip_prefix(r.as_str())
                .and_then(|tail| tail.strip_prefix('/'))
                .map(|branch| (&rest[..r.len()], branch))
        })
        .max_by_key(|(remote, _)| remote.len())
        .or_else(|| rest.split_once('/'))
        .unwrap_or((rest, ""))
}

/// The commit or tree a revision names, `None` when git has no such object.
/// Any other failure is an error.
pub(crate) fn rev_to_oid(exec: &Exec, work: &Path, spec: &str) -> Result<Option<Oid>> {
    let mut cmd = git_command(work);
    cmd.args(["rev-parse", "--verify", "--quiet", spec]);
    let out = exec.finish(cmd, &["rev-parse"], &[1])?;
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim();
    if text.is_empty() {
        Ok(None)
    } else {
        Oid::parse(text).map(Some)
    }
}

pub(crate) fn reject_option_like(rev: &str) -> Result<()> {
    if rev.is_empty()
        || rev.starts_with('-')
        || rev
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\0')
    {
        return Err(SnapshotError::InvalidRange(rev.to_string()));
    }
    Ok(())
}

/// The commit `rev` names, else the tree. `Err` when it names neither: an
/// unknown revision is a usage error, not an absent answer.
pub(crate) fn resolve_object(exec: &Exec, work: &Path, rev: &str) -> Result<Oid> {
    reject_option_like(rev)?;
    for suffix in ["^{commit}", "^{tree}"] {
        if let Some(oid) = rev_to_oid(exec, work, &format!("{rev}{suffix}"))? {
            return Ok(oid);
        }
    }
    let hint = if exec.is_shallow(work) {
        " (this is a shallow clone, so the revision may simply be missing: run \
         'git fetch --deepen=1' or check out with fetch-depth: 0)"
    } else {
        ""
    };
    Err(SnapshotError::InvalidRange(format!(
        "unknown revision '{rev}'{hint}"
    )))
}

/// The commit `rev` names; `None` when it names something else or nothing.
pub(crate) fn resolve_commit(exec: &Exec, work: &Path, rev: &str) -> Result<Option<Oid>> {
    reject_option_like(rev)?;
    rev_to_oid(exec, work, &format!("{rev}^{{commit}}"))
}

/// The tree an object (a commit or a tree) holds.
pub(crate) fn tree_of(exec: &Exec, work: &Path, object: &Oid) -> Result<Oid> {
    rev_to_oid(exec, work, &format!("{object}^{{tree}}"))?.ok_or_else(|| {
        SnapshotError::InvalidRange(format!("{object} names neither a commit nor a tree"))
    })
}

const SHA1_EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904"; // pragma: allowlist secret
const SHA256_EMPTY_TREE: &str = "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321"; // pragma: allowlist secret

/// The oid of the empty tree, derived from the repository's hash algorithm.
pub(crate) fn empty_tree_oid(exec: &Exec, work: &Path) -> Result<Oid> {
    let format = exec.text(work, &["rev-parse", "--show-object-format"])?;
    Oid::parse(if format.trim() == "sha256" {
        SHA256_EMPTY_TREE
    } else {
        SHA1_EMPTY_TREE
    })
}

/// The empty tree's id as text, the base of a window older than the
/// repository: every file is then genuinely new within it. A repository whose
/// object format git cannot state is an error, not sha1.
pub fn empty_tree_id(root: &Path) -> Result<String> {
    Ok(empty_tree_oid(&Exec::current(), root)?.as_str().to_string())
}

/// The commit a revision names, for provenance: the ref may move, the id does
/// not. `None` when it names no commit (an unknown ref, an unborn branch, a
/// string git would read as an option); a failed read, an interruption
/// included, is an `Err`.
pub fn rev_oid(root: &Path, rev: &str) -> Result<Option<String>> {
    if reject_option_like(rev).is_err() {
        return Ok(None);
    }
    Ok(
        rev_to_oid(&Exec::current(), root, &format!("{rev}^{{commit}}"))?
            .map(|oid| oid.as_str().to_string()),
    )
}

/// The index captured once as a tree: every current-side byte of the run
/// comes from `tree`, every old one from `base`, whatever the disk or a later
/// `git add` says. `head` is the commit the snapshot sits on, for the history
/// channels; `None` on an unborn branch.
#[derive(Clone, Debug)]
pub struct StagedSnapshot {
    pub base: String,
    pub tree: String,
    pub head: Option<String>,
}

/// One `git add`/`git rm` of a command line, run in its own directory.
pub type PlanStep = (PathBuf, Vec<String>);

/// The tree a command line will hand to git, built without touching the index
/// or the working tree: a copy of the index in the git directory (a linked
/// worktree has its own, a split index keeps its shared part beside it), the
/// line's staging commands replayed against the copy, then `write-tree`. The
/// steps run git itself, so pathspecs, ignores and clean filters resolve as
/// they will when the line runs.
pub fn capture_planned(root: &Path, steps: &[PlanStep]) -> Result<StagedSnapshot> {
    let exec = Exec::current();
    let out = exec.text(
        root,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )?;
    let index = PathBuf::from(out.trim());
    let scratch = ScratchIndex(index.with_file_name(format!(
        "diffctx-plan-{}-{}.index",
        std::process::id(),
        SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed)
    )));
    let head = rev_oid(root, "HEAD")?;
    if index.try_exists()? {
        std::fs::copy(&index, &scratch.0)?;
    }
    for (dir, args) in steps {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut cmd = git_command(dir);
        cmd.env("GIT_INDEX_FILE", &scratch.0).args(&args);
        exec.finish(cmd, &args, &[])?;
    }
    let mut cmd = git_command(root);
    cmd.env("GIT_INDEX_FILE", &scratch.0).arg("write-tree");
    let written = exec.finish(cmd, &["write-tree"], &[])?;
    let tree = String::from_utf8_lossy(&written.stdout).trim().to_string();
    let base = match &head {
        Some(head) => head.clone(),
        None => empty_tree_id(root)?,
    };
    Ok(StagedSnapshot { base, tree, head })
}

/// The work tree's root, resolved from `path` (which may be a subdirectory).
/// `git diff` and `git cat-file` paths are reported relative to the work tree,
/// not to an arbitrary `-C` cwd: a run with `path` still a subdirectory
/// produced zero fragments because file lookups were double-prefixed
/// (`src/src/app.py`). `None` when `path` has no work tree (not a repository,
/// or a bare one); a read git could not answer is an `Err`.
pub fn find_toplevel(path: &Path) -> Result<Option<PathBuf>> {
    let exec = Exec::current();
    let out = match exec.text(path, &["rev-parse", "--show-toplevel"]) {
        Ok(out) => out,
        Err(e) if e.git_status() == Some(FATAL_STATUS) => {
            return if holds_no_repository(path) || is_outside_work_tree(&exec, path) {
                Ok(None)
            } else {
                Err(e)
            };
        }
        Err(e) => return Err(e),
    };
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    // Git on Windows answers `D:/a/repo`, while every path canonicalised later
    // reads `D:\a\repo\x`. Paths are compared as strings downstream, so a
    // root in git's spelling silently split one file into two keys.
    Ok(Some(dunce::canonicalize(trimmed).unwrap_or_else(|_| {
        Path::new(trimmed).components().collect()
    })))
}

/// A repository whose git directory or bare root is where `path` stands: git
/// answers `false` to the question a bare repository or a `.git` directory
/// cannot answer with a top level.
fn is_outside_work_tree(exec: &Exec, path: &Path) -> bool {
    exec.text(path, &["rev-parse", "--is-inside-work-tree"])
        .is_ok_and(|answer| answer.trim() == "false")
}

/// `Err` when git could not answer at all, a timeout above all: collapsing that
/// into "not a repository" sent the reader after the wrong problem. `false`
/// only when git ran and said no.
pub fn is_git_repo(path: &Path) -> Result<bool> {
    match Exec::current().text(path, &["rev-parse", "--git-dir"]) {
        Ok(_) => Ok(true),
        // Any other failure (a `[core` line in `.git/config`, dubious
        // ownership in a container, a path that does not exist) is git
        // refusing to start, and its own `fatal:` line is the diagnosis.
        Err(e) if refused_without_repository(&e, path) => Ok(false),
        Err(e) => Err(e),
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the snapshot contract takes ids by value"
)]
pub fn is_ancestor(root: &RepoRoot, a: Oid, b: Oid) -> Result<bool> {
    let exec = Exec::ambient();
    let mut cmd = git_command(&root.work_tree);
    cmd.args(["merge-base", "--is-ancestor", a.as_str(), b.as_str()]);
    let out = exec.finish(cmd, &["merge-base"], &[1])?;
    Ok(out.status.success())
}

/// The best common ancestor, `None` for histories with none in common.
pub(crate) fn merge_base_with(exec: &Exec, work: &Path, a: &Oid, b: &Oid) -> Result<Option<Oid>> {
    let mut cmd = git_command(work);
    cmd.args(["merge-base", a.as_str(), b.as_str()]);
    let out = exec.finish(cmd, &["merge-base"], &[1])?;
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim();
    if text.is_empty() {
        Ok(None)
    } else {
        Oid::parse(text).map(Some)
    }
}

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

/// A scratch index and git's lock beside it, removed however the capture ends.
struct ScratchIndex(PathBuf);

impl Drop for ScratchIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("index.lock"));
    }
}

/// The index captured once as a tree, through a copy.
///
/// `write-tree` on the real
/// one takes its lock (a concurrent `git add` then reads as an unmerged index)
/// and rewrites its cache tree. An index with unmerged entries has no tree to
/// analyse, and falling back to the working tree would answer a different
/// question, so it is an error naming the cause.
pub fn index_tree(root: &RepoRoot) -> Result<Oid> {
    index_tree_with(&Exec::ambient(), &root.work_tree)
}

pub(crate) fn index_tree_with(exec: &Exec, work: &Path) -> Result<Oid> {
    // A linked worktree has its own index, and a split index keeps its shared
    // part beside it: the copy lives in the same directory.
    let out = exec.text(
        work,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )?;
    let index = PathBuf::from(out.trim());
    let scratch = ScratchIndex(index.with_file_name(format!(
        "diffctx-index-{}-{}.index",
        std::process::id(),
        SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed)
    )));
    if index.try_exists()? {
        std::fs::copy(&index, &scratch.0)?;
    }
    let mut cmd = git_command(work);
    cmd.env("GIT_INDEX_FILE", &scratch.0)
        .args(["--no-optional-locks", "write-tree"]);
    let written = exec
        .finish(cmd, &["write-tree"], &[])
        .map_err(|e| match e {
            SnapshotError::GitRefused {
                subcommand,
                status,
                reason,
            } => SnapshotError::GitRefused {
                subcommand,
                status,
                reason: format!(
                    "cannot analyse the staged changes: the index cannot be written as a tree \
                     (unmerged entries? resolve them and stage the result) - {reason}"
                ),
            },
            e => e,
        })?;
    Oid::parse(&String::from_utf8_lossy(&written.stdout))
}

/// The reflog of `name` past `cursor`, and the cursor to ask from next time.
///
/// The kind of move (a commit, a checkout, a rebase step, "update by push") is
/// the evidence the hook weighs, which `rev-list` cannot give. A log shorter
/// than the cursor was rewritten or expired: that is an error, not "nothing
/// new".
#[allow(
    clippy::disallowed_methods,
    reason = "reads the file git itself names, outside the tree: the reflog is not a repository blob"
)]
pub fn reflog_since(
    root: &RepoRoot,
    name: &RefName,
    cursor: &Cursor,
) -> Result<(Vec<ReflogEntry>, Cursor)> {
    let exec = Exec::ambient();
    let log = format!("logs/{}", name.0);
    let out = exec.text(
        &root.work_tree,
        &["rev-parse", "--path-format=absolute", "--git-path", &log],
    )?;
    let data = match std::fs::read(out.trim()) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && cursor.0 == 0 => {
            return Ok((Vec::new(), Cursor(0)));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(SnapshotError::StaleCursor);
        }
        Err(e) => return Err(SnapshotError::Io(e)),
    };
    let from = usize::try_from(cursor.0).map_err(|_| SnapshotError::StaleCursor)?;
    if from > data.len() || (from > 0 && data[from - 1] != b'\n') {
        return Err(SnapshotError::StaleCursor);
    }
    let fresh = &data[from..];
    let complete = fresh.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let entries = String::from_utf8_lossy(&fresh[..complete])
        .lines()
        .map(parse_reflog_line)
        .collect::<Result<Vec<_>>>()?;
    Ok((entries, Cursor((from + complete) as u64)))
}

/// `<old> <new> <name> <<email>> <time> <zone>\t<action>: <subject>`
fn parse_reflog_line(line: &str) -> Result<ReflogEntry> {
    let malformed = || SnapshotError::Git(format!("malformed reflog line '{line}'"));
    let (head, message) = line.split_once('\t').unwrap_or((line, ""));
    let (old, rest) = head.split_once(' ').ok_or_else(malformed)?;
    let (new, rest) = rest.split_once(' ').ok_or_else(malformed)?;
    let mut tail = rest.rsplitn(3, ' ');
    let (_zone, time) = (tail.next(), tail.next().ok_or_else(malformed)?);
    let (action, subject) = message.split_once(": ").unwrap_or((message, ""));
    Ok(ReflogEntry {
        old: Oid::parse(old)?,
        new: Oid::parse(new)?,
        action: action.to_string(),
        subject: subject.to_string(),
        timestamp: time.parse().map_err(|_| malformed())?,
    })
}
