use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::FxHashSet;
use wait_timeout::ChildExt;

use crate::config::git;
use crate::types::DiffHunk;

// Thread-local, not a process global: the MCP server runs overlapping
// pipelines on worker threads, and a shared atomic let one request's short
// timeout kill another request's in-flight git subprocess (#210). Every git
// call runs on the thread that entered the pipeline, so the per-thread value
// is the per-run value.
thread_local! {
    static GIT_TIMEOUT_SECS: std::cell::Cell<u64> =
        const { std::cell::Cell::new(git::DEFAULT_TIMEOUT_SECONDS) };
}

pub fn set_git_timeout(secs: u64) {
    GIT_TIMEOUT_SECS.with(|c| c.set(secs));
    crate::snapshot::set_git_timeout(secs);
}

// PID alone is not unique within a process: the MCP server runs each tool
// body on its own worker thread, so two overlapping pipelines can write a temp
// file under the same PID. The counter makes every call's temp path unique
// regardless of thread interleaving.
static TEMP_EXCLUDES_COUNTER: AtomicU64 = AtomicU64::new(0);

fn git_timeout() -> u64 {
    GIT_TIMEOUT_SECS.with(|c| c.get())
}
// The prefix and color flags are not cosmetic: the diff parser keys off the
// literal `--- a/` / `+++ b/` headers, so a user's `diff.noprefix`,
// `diff.mnemonicPrefix`, `diff.srcPrefix`/`dstPrefix` or `color.ui=always`
// silently reduced every run to zero fragments and an empty `changed_files`.
// `diff.interHunkContext` merges hunks across untouched lines, so functions
// between two edits were emitted as changed; pinned to zero for the same
// reason `--unified=0` is passed explicitly.
const SAFE_DIFF_FLAGS: &[&str] = &[
    "--no-textconv",
    "--no-ext-diff",
    "--no-color",
    "--src-prefix=a/",
    "--dst-prefix=b/",
    "--inter-hunk-context=0",
];

static HUNK_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@").unwrap());

static RANGE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s*(\S+?)(\.\.\.?)(\S*?)\s*$").unwrap());

// Neither side of a range may begin with `-`: a leading dash would be parsed
// by git as an option rather than a revision, so a caller-supplied range like
// `--ext-diff` or `--textconv` would re-enable the very filters SAFE_DIFF_FLAGS
// disables and run repo-configured commands. Refs that begin with a dash are
// unaddressable on a git command line anyway, so nothing legitimate is lost.
// `#`, `+`, `%`, `,` and `=` are legal in a ref name (`v1.2.3+build.7`,
// `release,2026`); the gate is the leading dash and the separators, not the
// alphabet.
static SAFE_RANGE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"^[a-zA-Z0-9_.^~/@{}#+%,=][a-zA-Z0-9_.^~/@{}#+%,=\-]*(\.\.\.?([a-zA-Z0-9_.^~/@{}#+%,=][a-zA-Z0-9_.^~/@{}#+%,=\-]*)?)?$",
    )
    .unwrap()
});

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("{0}")]
    CommandFailed(String),
    #[error("not a git repository: {0}")]
    NotARepo(PathBuf),
    #[error("invalid diff range: {0}")]
    InvalidRange(String),
    #[error(
        "invalid duration '{0}': a window is whole <number><unit> components such as 24h, 8d, 90min or 1h30m"
    )]
    InvalidDuration(String),
    #[error(
        "invalid path '{0}': paths must stay inside the repository and carry no pathspec magic"
    )]
    InvalidPath(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("timeout after {0}s")]
    Timeout(u64),
    #[error("cancelled")]
    Cancelled,
    #[error("cat-file protocol: {0}")]
    Protocol(String),
    #[error("Path not found: {0}")]
    MissingObject(String),
    #[error("cat-file: blob too large ({size} bytes): {spec}")]
    BlobTooLarge { size: usize, spec: String },
}

impl GitError {
    /// The caller misspoke — a usage error, not a git or environment failure.
    pub fn is_usage(&self) -> bool {
        matches!(
            self,
            Self::InvalidRange(_) | Self::InvalidDuration(_) | Self::InvalidPath(_)
        )
    }

    /// The run ran out of time or was withdrawn: what follows is partial,
    /// never a finding about the file that was being read.
    pub fn is_interruption(&self) -> bool {
        matches!(self, Self::Timeout(_) | Self::Cancelled)
    }
}

pub type Result<T> = std::result::Result<T, GitError>;

/// Repo-relative pathspecs that narrow every git query of a run. Validated
/// once: a leading `-` would be an option, a leading `:` pathspec magic, and
/// anything escaping the root is not a path in this repository.
pub fn validate_pathspec(paths: &[String]) -> Result<()> {
    for p in paths {
        if p.is_empty()
            || p.starts_with('-')
            || p.starts_with(':')
            || p.chars().any(|c| c.is_control())
            || !crate::paths::contains_lexically(Path::new(p))
        {
            return Err(GitError::InvalidPath(p.clone()));
        }
    }
    Ok(())
}

fn validate_diff_range(diff_range: &str) -> Result<()> {
    let trimmed = diff_range.trim();
    // Reject leading-dot ranges like `..origin/main` (audit X16): the regex
    // character class allows `.`, so a string of only dots/identifier-chars
    // passes as a "ref" before being rejected by git itself with a less
    // informative `fatal: ambiguous argument`. Surface the dedicated error.
    if trimmed.starts_with('.') || trimmed.starts_with('/') {
        return Err(GitError::InvalidRange(diff_range.to_string()));
    }
    if !SAFE_RANGE_RE.is_match(trimmed) {
        return Err(GitError::InvalidRange(diff_range.to_string()));
    }
    // The regex alone cannot enforce this: its leading character class is
    // greedy over `.`, so it swallows the separator and never enters the
    // second-side group — `a..--ext-diff` matched. Split and check each side,
    // otherwise the documented "neither side may begin with a dash" gate is
    // dead code and only the later per-rev check stands between a crafted
    // range and an argv option.
    let separator = if trimmed.contains("...") { "..." } else { ".." };
    for side in trimmed.split(separator) {
        if !side.is_empty() {
            validate_rev(side).map_err(|_| GitError::InvalidRange(diff_range.to_string()))?;
        }
    }
    Ok(())
}

// The argv for every `git diff` this crate runs. Both controls it carries are
// security-relevant — SAFE_DIFF_FLAGS pins the parser against hostile repo-local
// config, `validate_diff_range` is the argv-injection gate whose own comment
// records a bypass that already shipped — and until now they were enforced by
// five copies of this sequence agreeing with each other.
fn diff_args<'a>(
    extra: &[&'a str],
    diff_range: Option<&'a str>,
    pathspec: &'a [String],
) -> Result<Vec<&'a str>> {
    let mut args: Vec<&str> = vec!["diff"];
    args.extend_from_slice(SAFE_DIFF_FLAGS);
    args.extend_from_slice(extra);
    if let Some(range) = diff_range {
        validate_diff_range(range)?;
        args.push(range);
    }
    if !pathspec.is_empty() {
        validate_pathspec(pathspec)?;
        args.push("--");
        args.extend(pathspec.iter().map(String::as_str));
    }
    Ok(args)
}

/// Second gate for the single revisions derived from a range (`base`, `head`),
/// covering the call sites that pass a rev straight into argv or into the
/// `cat-file --batch` request stream. A leading dash turns the rev into a git
/// option; whitespace and control characters (notably `\n`) would split one
/// batch request into two.
fn validate_rev(rev: &str) -> Result<()> {
    if rev.is_empty()
        || rev.starts_with('-')
        || rev
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\0')
    {
        return Err(GitError::InvalidRange(rev.to_string()));
    }
    Ok(())
}

static DURATION_PART_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)^(\d{1,9})\s*(weeks?|w|days?|d|hours?|hrs?|h|minutes?|mins?|m|seconds?|secs?|s)",
    )
    .unwrap()
});

/// `--diff 24h` / `8d` / `1h30m`: a Go-style duration, not a revision.
///
/// Only a whole spec of `<number><unit>` components counts; anything left over
/// makes the whole string a revision again, so `8dd` still reaches git as a ref.
fn parse_duration_seconds(spec: &str) -> Option<u64> {
    let mut rest = spec.trim();
    if rest.is_empty() {
        return None;
    }
    let mut total: u64 = 0;
    while !rest.is_empty() {
        let caps = DURATION_PART_RE.captures(rest)?;
        let count: u64 = caps[1].parse().ok()?;
        let unit_seconds = match caps[2].to_ascii_lowercase().as_str() {
            "w" | "week" | "weeks" => 7 * 24 * 3600,
            "d" | "day" | "days" => 24 * 3600,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            _ => 1,
        };
        total = total.checked_add(count.checked_mul(unit_seconds)?)?;
        rest = rest[caps[0].len()..].trim_start();
    }
    Some(total)
}

/// Every whitespace-separated component is `<number>[.fraction]<known unit>`.
/// A spec of this shape that `parse_duration_seconds` still rejects — a
/// fraction, a count too large, whitespace inside a component — is a typo of
/// the duration grammar, and telling the user "unknown git revision '1.5h'"
/// sends them to `git log` for a problem `git log` cannot show. `8dd` keeps
/// its revision reading: `dd` is not a unit.
static DURATION_SHAPE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)^\d+(\.\d+)?\s*(weeks?|w|days?|d|hours?|hrs?|h|minutes?|mins?|m|seconds?|secs?|s)(\s*\d+(\.\d+)?\s*(weeks?|w|days?|d|hours?|hrs?|h|minutes?|mins?|m|seconds?|secs?|s))*$",
    )
    .unwrap()
});

fn looks_like_duration(spec: &str) -> bool {
    DURATION_SHAPE_RE.is_match(spec)
}

/// The index captured once as a tree: every current-side byte of the run
/// comes from `tree`, every old one from `base`, whatever the disk or a
/// later `git add` says. `head` is the commit the snapshot sits on, for the
/// history channels; `None` on an unborn branch.
#[derive(Clone, Debug)]
pub struct StagedSnapshot {
    pub base: String,
    pub tree: String,
    pub head: Option<String>,
}

/// The public names for the staged index: `staged`, and git's own
/// `--cached`, accepted literally and nowhere else as an option.
pub fn is_staged_keyword(spec: &str) -> bool {
    matches!(spec.trim(), "staged" | "--cached")
}

/// One `git add`/`git rm` of a command line, run in its own directory.
pub type PlanStep = (PathBuf, Vec<String>);

/// The tree a command line will hand to git, built without touching the
/// index or the working tree: a copy of the index in the git directory (a
/// linked worktree has its own, a split index keeps its shared part beside
/// it), the line's staging commands replayed against the copy, then
/// `write-tree`. The steps run git itself, so pathspecs, ignores and clean
/// filters resolve as they will when the line runs.
pub fn capture_planned(repo_root: &Path, steps: &[PlanStep]) -> Result<StagedSnapshot> {
    let index = git_path(repo_root, "index")?;
    let scratch = ScratchIndex(index.with_file_name(format!(
        "diffctx-plan-{}-{}.index",
        std::process::id(),
        SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed)
    )));
    let head = rev_oid(repo_root, "HEAD");
    if index.exists() {
        std::fs::copy(&index, &scratch.0)?;
    }
    for (dir, args) in steps {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        run_git_on_index(dir, &scratch.0, &args)?;
    }
    let tree = run_git_on_index(repo_root, &scratch.0, &["write-tree"])?
        .trim()
        .to_string();
    let base = head.clone().unwrap_or_else(|| empty_tree_oid(repo_root));
    Ok(StagedSnapshot { base, tree, head })
}

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

/// A scratch index and git's lock beside it, removed however the plan ends.
struct ScratchIndex(PathBuf);

impl Drop for ScratchIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("index.lock"));
    }
}

/// `git rev-parse --git-path <name>` as an absolute path: in a linked
/// worktree `.git` is a file and the index lives elsewhere.
fn git_path(repo_root: &Path, name: &str) -> Result<PathBuf> {
    let out = run_git(repo_root, &["rev-parse", "--git-path", name])?;
    let path = PathBuf::from(out.trim());
    Ok(if path.is_absolute() {
        path
    } else {
        repo_root.join(path)
    })
}

/// A git command against another index file than the checkout's own, under
/// the same supervision as every other call.
fn run_git_on_index(dir: &Path, index: &Path, args: &[&str]) -> Result<String> {
    let child = git_command(dir)
        .env("GIT_INDEX_FILE", index)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let output = supervise(child)?;
    if !output.status.success() {
        return Err(command_failure(
            dir,
            args,
            &String::from_utf8_lossy(&output.stderr),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The oid of the empty tree, used as the base when the repository is younger
/// than the requested window: every file is then genuinely new within it.
/// Derived from the repo's hash algorithm rather than hardcoded to sha1.
pub fn empty_tree_oid(repo_root: &Path) -> String {
    const SHA1_EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904"; // pragma: allowlist secret
    const SHA256_EMPTY_TREE: &str =
        "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321"; // pragma: allowlist secret
    match run_git(repo_root, &["rev-parse", "--show-object-format"]) {
        Ok(format) if format.trim() == "sha256" => SHA256_EMPTY_TREE.to_string(),
        _ => SHA1_EMPTY_TREE.to_string(),
    }
}

/// The commit object id a revision names, for provenance: the ref may move,
/// the id does not. `None` for anything git cannot resolve to a commit.
pub fn rev_oid(repo_root: &Path, rev: &str) -> Option<String> {
    if validate_rev(rev).is_err() {
        return None;
    }
    let spec = format!("{rev}^{{commit}}");
    run_git(repo_root, &["rev-parse", "--verify", "--quiet", &spec])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn rev_exists(repo_root: &Path, rev: &str) -> bool {
    if validate_rev(rev).is_err() {
        return false;
    }
    let spec = format!("{rev}^{{commit}}");
    run_git(repo_root, &["rev-parse", "--verify", "--quiet", &spec]).is_ok()
}

/// The last commit before a `--diff 24h` window, the empty tree when the
/// repository is younger: `None` when `spec` is no window (a revision, a
/// range, the staged keyword). A ref that happens to look like a duration (a
/// branch `24h`, an abbreviated sha `24d`) keeps its git meaning. Only the
/// phrasing of messages and the legacy spelling of the analysed range need
/// it; the range itself is resolved once by the snapshot layer.
pub fn duration_base(repo_root: &Path, spec: &str) -> Result<Option<String>> {
    let trimmed = spec.trim();
    if is_staged_keyword(trimmed) || split_diff_range(trimmed).0.is_some() {
        return Ok(None);
    }
    let parsed = parse_duration_seconds(trimmed);
    if parsed.is_none() && !looks_like_duration(trimmed) {
        return Ok(None);
    }
    if rev_exists(repo_root, trimmed) {
        return Ok(None);
    }
    let Some(seconds) = parsed else {
        return Err(GitError::InvalidDuration(trimmed.to_string()));
    };
    // git owns the calendar arithmetic (local timezone, DST) via approxidate.
    let before = format!("--before={seconds} seconds ago");
    let base = run_git(repo_root, &["rev-list", "-1", &before, "HEAD", "--"])
        .map(|out| out.trim().to_string())
        .unwrap_or_default();
    Ok(Some(if base.is_empty() {
        empty_tree_oid(repo_root)
    } else {
        base
    }))
}

/// Always targets the repo via `-C`, scrubbed of the repo-locating variables a
/// parent git hook exports. The snapshot module builds it; there is one.
pub fn git_command(repo_root: &Path) -> Command {
    crate::snapshot::git_command(repo_root)
}

/// `true` when the repository is a shallow clone: the one situation in which
/// "unknown revision 'HEAD~1'" is not a typo but a missing history.
fn is_shallow(repo_root: &Path) -> bool {
    run_git(repo_root, &["rev-parse", "--is-shallow-repository"])
        .map(|out| out.trim() == "true")
        .unwrap_or(false)
}

fn command_failure(repo_root: &Path, args: &[&str], stderr: &str) -> GitError {
    let subcommand = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .copied()
        .unwrap_or("command");
    let reason = stderr
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("fatal:") || l.starts_with("error:"))
        .or_else(|| stderr.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("unknown error");
    if reason.contains("unknown revision") && is_shallow(repo_root) {
        return GitError::CommandFailed(format!(
            "git {subcommand} failed: {reason} (this is a shallow clone, so the revision may \
             simply be missing: run 'git fetch --deepen=1' or check out with fetch-depth: 0)"
        ));
    }
    GitError::CommandFailed(format!("git {subcommand} failed: {reason}"))
}

pub fn run_git(repo_root: &Path, args: &[&str]) -> Result<String> {
    run_git_bytes(repo_root, args).map(|out| String::from_utf8_lossy(&out).into_owned())
}

/// `run_git` for output that must reach git again byte for byte: a patch
/// through a lossy decode no longer applies to a non-UTF-8 file.
pub fn run_git_bytes(repo_root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = git_command(repo_root);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());

    let child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            GitError::CommandFailed("git is not installed or not in PATH".into())
        } else {
            GitError::Io(e)
        }
    })?;

    let output = supervise(child)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(command_failure(repo_root, args, &stderr));
    }

    Ok(output.stdout)
}

/// What one git child may spend: the per-call ceiling cut to what the run
/// has left, so N children cannot each spend a full `--timeout`, plus the
/// run's cancellation.
#[derive(Clone)]
struct ChildLimits {
    deadline: Instant,
    cancel: Option<Arc<AtomicBool>>,
}

/// The slice a blocked wait sleeps before it looks at the cancellation flag
/// again; a finished child is noticed at once whatever the slice.
const POLL_SLICE: Duration = Duration::from_millis(100);
/// How long reaping a killed child may take before it is given up on.
const REAP_GRACE: Duration = Duration::from_secs(2);
const MAX_STDERR_BYTES: usize = 64 * 1024;

type RunLimits = (Option<Instant>, Arc<AtomicBool>);

impl ChildLimits {
    fn current() -> Self {
        Self::of_run(crate::resource::current_io_limits())
    }

    fn of_run(run: Option<RunLimits>) -> Self {
        let ceiling = Instant::now() + Duration::from_secs(git_timeout());
        match run {
            Some((io_deadline, cancel)) => Self {
                deadline: io_deadline.map_or(ceiling, |d| d.min(ceiling)),
                cancel: Some(cancel),
            },
            None => Self {
                deadline: ceiling,
                cancel: None,
            },
        }
    }

    #[cfg(test)]
    fn within(timeout: Duration) -> Self {
        Self {
            deadline: Instant::now() + timeout,
            cancel: None,
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    }

    /// Why the child may not go on, if it may not.
    fn exhausted(&self) -> Option<GitError> {
        if self.cancelled() {
            Some(GitError::Cancelled)
        } else if Instant::now() >= self.deadline {
            Some(self.timeout_error())
        } else {
            None
        }
    }

    fn timeout_error(&self) -> GitError {
        GitError::Timeout(git_timeout())
    }

    fn next_slice(&self) -> Duration {
        self.deadline
            .saturating_duration_since(Instant::now())
            .min(POLL_SLICE)
    }
}

/// Kills the child and everything it started: on Unix every git child leads
/// its own process group, so a descendant holding the stdout pipe open (a
/// lazy fetch, a hook) dies with it instead of keeping a reader blocked.
fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        if let Ok(pgid) = i32::try_from(child.id()) {
            // SAFETY: kill(2) with a negative pid signals the group the child
            // leads; it touches no memory of ours.
            #[allow(unsafe_code)]
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait_timeout(REAP_GRACE);
}

fn spawn_pipe_reader(
    mut pipe: impl Read + Send + 'static,
    keep: usize,
) -> mpsc::Receiver<std::io::Result<Vec<u8>>> {
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut chunk = vec![0u8; 65536];
        let result = loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break Ok(kept),
                Ok(n) => {
                    let room = keep.saturating_sub(kept.len());
                    kept.extend_from_slice(&chunk[..n.min(room)]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => break Err(e),
            }
        };
        let _ = tx.send(result);
    });
    rx
}

fn await_pipe(
    rx: Option<mpsc::Receiver<std::io::Result<Vec<u8>>>>,
    limits: &ChildLimits,
) -> Result<Vec<u8>> {
    let Some(rx) = rx else {
        return Ok(Vec::new());
    };
    loop {
        if let Some(stop) = limits.exhausted() {
            return Err(stop);
        }
        match rx.recv_timeout(limits.next_slice()) {
            Ok(read) => return read.map_err(GitError::Io),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            // A reader that vanished used to read as an empty buffer — an
            // unreadable `git diff` then looked exactly like an empty diff.
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(GitError::Io(std::io::Error::other(
                    "git pipe reader thread panicked",
                )));
            }
        }
    }
}

fn supervise(child: Child) -> Result<std::process::Output> {
    supervise_within(child, &ChildLimits::current())
}

#[cfg(test)]
fn wait_with_timeout(
    child: Child,
    timeout: Duration,
    _args: &[&str],
) -> Result<std::process::Output> {
    supervise_within(child, &ChildLimits::within(timeout))
}

/// Waits for the child, its stdout and its stderr together under one
/// deadline. The pipes are read on their own threads and awaited through
/// channels, so neither a stalled child nor a descendant that inherited a
/// pipe can hold the caller past the deadline.
fn supervise_within(mut child: Child, limits: &ChildLimits) -> Result<std::process::Output> {
    let stdout = child
        .stdout
        .take()
        .map(|s| spawn_pipe_reader(s, usize::MAX));
    let stderr = child
        .stderr
        .take()
        .map(|s| spawn_pipe_reader(s, MAX_STDERR_BYTES));

    // A zero ceiling is spent before the call, so the verdict cannot be left
    // to whether the child happens to exit first: on a fast machine
    // `wait_timeout(0)` reaps an already-finished git and reports success,
    // which turned `timeout=0` into "no deadline at all" (seen on an ARM
    // runner, 2026-09-17).
    let status = loop {
        if let Some(stop) = limits.exhausted() {
            kill_tree(&mut child);
            return Err(stop);
        }
        if let Some(status) = child.wait_timeout(limits.next_slice())? {
            break status;
        }
    };
    let output = await_pipe(stdout, limits).and_then(|stdout| {
        await_pipe(stderr, limits).map(|stderr| std::process::Output {
            status,
            stdout,
            stderr,
        })
    });
    if output.is_err() {
        kill_tree(&mut child);
    }
    output
}

/// `Err` when git could not answer at all — a timeout above all. Collapsing
/// that into "not a repository" is how a `timeout` too small to let
/// `rev-parse` finish reported a perfectly good repo as not one, sending the
/// reader after the wrong problem entirely.
pub fn is_git_repo(path: &Path) -> std::result::Result<bool, GitError> {
    crate::resource::memoized("is_git_repo", path, || probe_git_repo(path))
}

fn probe_git_repo(path: &Path) -> std::result::Result<bool, GitError> {
    match run_git(path, &["rev-parse", "--git-dir"]) {
        Ok(_) => Ok(true),
        // git ran and said no: that is the one honest "false". Any other
        // failure — a `[core` line in `.git/config`, dubious ownership in a
        // container, a path that does not exist — is git refusing to start,
        // and its own `fatal:` line is the diagnosis; "not a repository"
        // would send the reader after the wrong problem.
        Err(GitError::CommandFailed(reason)) if reason.contains("not a git repository") => {
            Ok(false)
        }
        // git did not run at all (not on PATH, permission denied) or did not
        // answer in time — neither says anything about the directory.
        Err(e) => Err(e),
    }
}

/// Resolves the actual working-tree root for `path`, which may be a
/// subdirectory of the repository. `git diff`/`git cat-file` paths are
/// always reported relative to this root, not to an arbitrary `-C` cwd -
/// running the pipeline with `path` still set to a subdirectory silently
/// produces zero fragments because file lookups get double-prefixed
/// (e.g. `src/src/app.py`).
pub fn find_toplevel(path: &Path) -> Option<PathBuf> {
    let out = run_git(path, &["rev-parse", "--show-toplevel"]).ok()?;
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Git on Windows answers `D:/a/repo`, while every path canonicalised
    // later reads `D:\a\repo\x`. Paths are compared as strings downstream,
    // so a root in git's spelling silently split one file into two keys.
    Some(dunce::canonicalize(trimmed).unwrap_or_else(|_| Path::new(trimmed).components().collect()))
}

pub fn get_diff_text(
    repo_root: &Path,
    diff_range: Option<&str>,
    pathspec: &[String],
) -> Result<String> {
    // The context lines are what anchors a run and tells layout from
    // content; a `diff.context` in the user's config must not move them.
    let args = diff_args(&["--unified=3"], diff_range, pathspec)?;
    run_git(repo_root, &args)
}

pub(crate) fn unquote_c_style(quoted: &str) -> String {
    if !(quoted.starts_with('"') && quoted.ends_with('"')) {
        return quoted.to_string();
    }

    let raw = &quoted[1..quoted.len() - 1];
    let bytes = raw.as_bytes();
    let mut result: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let nxt = bytes[i + 1];
            match nxt {
                b't' => {
                    result.push(b'\t');
                    i += 2;
                }
                b'n' => {
                    result.push(b'\n');
                    i += 2;
                }
                b'r' => {
                    result.push(b'\r');
                    i += 2;
                }
                b'b' => {
                    result.push(0x08);
                    i += 2;
                }
                b'f' => {
                    result.push(0x0C);
                    i += 2;
                }
                b'v' => {
                    result.push(0x0B);
                    i += 2;
                }
                b'a' => {
                    result.push(0x07);
                    i += 2;
                }
                b'\\' => {
                    result.push(b'\\');
                    i += 2;
                }
                b'"' => {
                    result.push(b'"');
                    i += 2;
                }
                b'0'..=b'7'
                    if i + 3 < bytes.len()
                        && bytes[i + 2].is_ascii_digit()
                        && bytes[i + 2] <= b'7'
                        && bytes[i + 3].is_ascii_digit()
                        && bytes[i + 3] <= b'7' =>
                {
                    let val = (nxt - b'0') * 64 + (bytes[i + 2] - b'0') * 8 + (bytes[i + 3] - b'0');
                    result.push(val);
                    i += 4;
                }
                _ => {
                    result.push(b'\\');
                    i += 1;
                }
            }
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }

    String::from_utf8(result).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Resolves a diff-header path against the repository root, or `None` if it
/// does not stay inside it.
///
/// `Path::starts_with` compares components, not locations, and
/// `canonicalize` fails for any path that does not exist — which is the
/// normal case for the old side of a deletion, and for a crafted header
/// naming a file that was never there. The previous guard fell back to the
/// lexically joined path in that case, and `<root>/../../escape.py` starts
/// with `<root>` component-wise, so the check passed and the header was
/// accepted. It only ever failed on macOS, where the temp root canonicalizes
/// through `/var -> /private/var` and the two spellings stop matching.
///
/// A `..` component is therefore rejected outright, before any of this:
/// git does not emit one for a tracked path, so nothing legitimate needs it,
/// and downstream `strip_prefix` guards are lexical for exactly the same
/// reason. Absolute and rooted paths are refused for the same reason —
/// `Path::join` with such an argument discards the root entirely.
pub(crate) fn resolve_in_repo(repo_root: &Path, rel_path: &str) -> Option<PathBuf> {
    let rel = Path::new(rel_path);
    // `has_root` too: on Windows `\etc\x` is rooted but not absolute, and
    // `Path::join` still replaces everything after the drive with it.
    if rel.has_root()
        || rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }

    let joined = crate::paths::repo_join(repo_root, rel_path);
    // Compare like for like. Falling back to the lexical spelling as an
    // *alternative* to the canonical check (rather than only when
    // canonicalization is impossible) would make the canonical check dead: with
    // `..` already excluded, `joined` always starts with `repo_root`, so an
    // in-repo symlink pointing outside the tree would resolve outside and still
    // be accepted.
    match dunce::canonicalize(&joined) {
        Ok(resolved) => {
            let resolved_root =
                dunce::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
            if !resolved.starts_with(&resolved_root) {
                return None;
            }
        }
        // Unresolvable: the old side of a deletion, or a path that never
        // existed. `..` and absolute components are rejected above, so the
        // lexical join cannot leave the root and needs no further check —
        // which is also why the canonical root's spelling (`/var` vs
        // `/private/var`) cannot cause a false rejection here.
        Err(_) => {}
    }
    Some(joined)
}

pub(crate) fn parse_path_line(line: &str, repo_root: &Path) -> (&'static str, Option<PathBuf>) {
    if line.starts_with("--- /dev/null") {
        return ("old", None);
    }
    if line.starts_with("+++ /dev/null") {
        return ("new", None);
    }

    let (kind, rel_path) = if let Some(rest) = line.strip_prefix("--- a/") {
        ("old", rest.trim().to_string())
    } else if let Some(rest) = line.strip_prefix("+++ b/") {
        ("new", rest.trim().to_string())
    } else if let Some(rest) = line.strip_prefix("--- ").filter(|r| r.starts_with("\"a/")) {
        let unquoted = unquote_c_style(rest.trim());
        (
            "old",
            unquoted.strip_prefix("a/").unwrap_or(&unquoted).to_string(),
        )
    } else if let Some(rest) = line.strip_prefix("+++ ").filter(|r| r.starts_with("\"b/")) {
        let unquoted = unquote_c_style(rest.trim());
        (
            "new",
            unquoted.strip_prefix("b/").unwrap_or(&unquoted).to_string(),
        )
    } else {
        return ("", None);
    };

    match resolve_in_repo(repo_root, &rel_path) {
        Some(path) => (kind, Some(path)),
        None => ("", None),
    }
}

fn parse_hunk_header(caps: &regex::Captures, path: &Path) -> Option<DiffHunk> {
    // Adversarial-diff hardening: integers parsed from the hunk regex are
    // small ASCII digits matched by `\d+`, so `parse::<u32>` can only fail
    // on overflow (e.g. lines > 2^32). Skip such hunks instead of crashing
    // the host process — they are degenerate and have no useful semantics.
    let old_start: u32 = caps[1].parse().ok()?;
    let old_len: u32 = match caps.get(2) {
        Some(m) => m.as_str().parse().ok()?,
        None => 1,
    };
    let new_start: u32 = caps[3].parse().ok()?;
    let new_len: u32 = match caps.get(4) {
        Some(m) => m.as_str().parse().ok()?,
        None => 1,
    };

    Some(DiffHunk {
        path: Arc::from(path.to_string_lossy().as_ref()),
        new_start,
        new_len,
        old_start,
        old_len,
    })
}

pub fn parse_diff(
    repo_root: &Path,
    diff_range: Option<&str>,
    pathspec: &[String],
) -> Result<Vec<DiffHunk>> {
    let args = diff_args(&["--unified=0", "-M"], diff_range, pathspec)?;

    let output = run_git(repo_root, &args)?;
    Ok(parse_hunks_from_diff_output(&output, repo_root))
}

pub(crate) fn parse_hunks_from_diff_output(output: &str, repo_root: &Path) -> Vec<DiffHunk> {
    let mut hunks = Vec::new();
    let mut old_path: Option<PathBuf> = None;
    let mut new_path: Option<PathBuf> = None;

    for line in output.lines() {
        // `diff --git` is the per-file boundary git always emits, so clearing
        // here is what keeps one file's hunks from ever being charged to the
        // previous one. Relying on the `---`/`+++` pair alone leaves the
        // previous file's paths live whenever this file's header is not
        // understood or is rejected — a path escaping the repo root returns
        // `("", None)`, which is a refusal, not "keep the last path".
        if line.starts_with("diff --git ") {
            old_path = None;
            new_path = None;
            continue;
        }

        let (path_type, path) = parse_path_line(line, repo_root);
        match path_type {
            "old" => {
                old_path = path;
                continue;
            }
            "new" => {
                new_path = path;
                continue;
            }
            _ => {}
        }

        if let Some(caps) = HUNK_RE.captures(line) {
            let current_path = new_path.as_deref().or(old_path.as_deref());
            if let Some(p) = current_path {
                if let Some(hunk) = parse_hunk_header(&caps, p) {
                    hunks.push(hunk);
                }
            }
        }
    }

    hunks
}

pub fn run_git_z(repo_root: &Path, args: &[&str]) -> Result<Vec<String>> {
    let output = run_git(repo_root, args)?;
    Ok(output
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect())
}

/// The names a diff touches, read from one `--name-status -M` listing: every
/// changed path (a rename by its destination), the deletions, and the rename
/// pairs. They were three listings, two of them run twice per run.
#[derive(Default)]
pub struct DiffNames {
    pub changed: Vec<PathBuf>,
    pub deleted: FxHashSet<PathBuf>,
    renamed: Vec<(String, String)>,
}

impl DiffNames {
    /// Rename *source* paths, canonicalized. These no longer exist on disk and
    /// cannot be fragmented, so the pipeline excludes them from the changed set.
    /// The rename destinations need no special handling: they exist on HEAD and
    /// reach the universe through the ordinary changed-file path.
    pub fn renamed_sources(&self, repo_root: &Path) -> FxHashSet<PathBuf> {
        self.renamed
            .iter()
            .map(|(old, _)| {
                let joined = crate::paths::repo_join(repo_root, old);
                dunce::canonicalize(&joined).unwrap_or(joined)
            })
            .collect()
    }

    /// Rename pairs as repo-relative display paths (`old -> new`), for the
    /// output header: the pairing kept, nothing canonicalized (the old path no
    /// longer exists on disk).
    pub fn rename_pairs(&self) -> Vec<(String, String)> {
        self.renamed
            .iter()
            .map(|(old, new)| {
                (
                    crate::paths::to_posix_display(std::borrow::Cow::Borrowed(old)),
                    crate::paths::to_posix_display(std::borrow::Cow::Borrowed(new)),
                )
            })
            .collect()
    }
}

pub fn diff_names(
    repo_root: &Path,
    diff_range: Option<&str>,
    pathspec: &[String],
) -> Result<DiffNames> {
    let args = diff_args(&["--name-status", "-M", "-z"], diff_range, pathspec)?;
    let output = run_git(repo_root, &args)?;
    let parts: Vec<&str> = output.split('\0').collect();
    // Lexical `root.join(p)`, never the canonical target: `canonicalize()` on a
    // symlink returns what it POINTS AT, so an untracked `evil -> /etc/shadow`
    // entered the changed-file list under an absolute out-of-root path. That
    // breaks three things at once — the path stops naming an object in the
    // repository, the ignore and secret policies are then applied to the wrong
    // name, and the working-tree line counter reads outside the checkout.
    //
    // Containment is checked lexically and NOT by canonicalising: these names
    // need not exist on disk. A deleted file is gone by definition and a bare
    // clone has no working tree, so an existence test empties both lists. The
    // symlink itself is caught where the file is actually read.
    let inside = |p: &str| {
        crate::paths::contains_lexically(Path::new(p))
            .then(|| crate::paths::repo_join(repo_root, p))
    };
    let mut names = DiffNames::default();
    let mut i = 0;
    while i < parts.len() {
        let status = parts[i];
        if status.is_empty() {
            i += 1;
            continue;
        }
        // `R<similarity>\0old\0new` and `C…` carry two paths, every other
        // status one; a rename without a destination is no rename.
        if status.starts_with(['R', 'C']) {
            let (old, new) = (
                parts.get(i + 1).copied().unwrap_or(""),
                parts.get(i + 2).copied().unwrap_or(""),
            );
            if !new.is_empty() {
                names.changed.extend(inside(new));
            }
            if status.starts_with('R') && !old.is_empty() && !new.is_empty() {
                names.renamed.push((old.to_string(), new.to_string()));
            }
            i += 3;
        } else {
            if let Some(path) = parts.get(i + 1).filter(|p| !p.is_empty()) {
                let joined = inside(path);
                if status.starts_with('D') {
                    names.deleted.extend(joined.clone());
                }
                names.changed.extend(joined);
            }
            i += 2;
        }
    }
    Ok(names)
}

/// `(base, head)` of a two-sided range; `(None, None)` for a single revision,
/// which git compares with the working tree. An open right side (`A..`,
/// `A...`) is `A..HEAD` in git's own reading — both sides committed — so the
/// head is HEAD, not the working tree: bodies then come from the commit the
/// hunks describe, not from the base the range starts at.
pub fn split_diff_range(range: &str) -> (Option<String>, Option<String>) {
    match RANGE_RE.captures(range) {
        None => (None, None),
        Some(caps) => {
            let base = caps
                .get(1)
                .map(|m| m.as_str().trim().to_string())
                .filter(|s| !s.is_empty());
            let head = caps
                .get(3)
                .map(|m| m.as_str().trim().to_string())
                .filter(|s| !s.is_empty())
                .or_else(|| Some("HEAD".to_string()));
            (base, head)
        }
    }
}

/// Every commit message in `base..head` — subject and body — newest first,
/// at most `limit` commits, each body cut at `max_chars`. A range used to be
/// titled by the last commit's subject alone; the body is where a person
/// says why, and on a GitOps branch the last commit is the image updater's.
pub fn commit_messages(
    repo_root: &Path,
    base: &str,
    head: &str,
    limit: usize,
    max_chars: usize,
) -> Vec<String> {
    // `X..X` holds no commit: a working-tree diff asks it of `HEAD..HEAD`.
    if base == head || validate_rev(base).is_err() || validate_rev(head).is_err() {
        return Vec::new();
    }
    let range = format!("{base}..{head}");
    let max = format!("--max-count={limit}");
    // `%B` is the raw message; `%x1e` (record separator) cannot occur in one.
    match run_git(repo_root, &["log", "--format=%B%x1e", &max, &range, "--"]) {
        Ok(out) => out
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
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Commits in `base..head`: the list above stops at `limit`, and a reader
/// told "20 commits" must learn when the range held more.
pub fn commit_count(repo_root: &Path, base: &str, head: &str) -> usize {
    if base == head || validate_rev(base).is_err() || validate_rev(head).is_err() {
        return 0;
    }
    let range = format!("{base}..{head}");
    run_git(repo_root, &["rev-list", "--count", &range, "--"])
        .ok()
        .and_then(|out| out.trim().parse().ok())
        .unwrap_or(0)
}

pub fn get_commit_message(repo_root: &Path, rev: &str) -> Result<String> {
    if validate_rev(rev).is_err() {
        return Ok(String::new());
    }
    match run_git(repo_root, &["log", "-1", "--format=%s%n%b", rev]) {
        Ok(s) => Ok(s.trim().to_string()),
        Err(_) => Ok(String::new()),
    }
}

/// The blob ids `git add` would store for `paths` (repo-relative), in order:
/// the repository's filters (line endings, LFS) applied, nothing written.
pub fn hash_object_paths(repo_root: &Path, paths: &[String]) -> Result<Vec<String>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut child = git_command(repo_root)
        .args(["hash-object", "--stdin-paths"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| GitError::CommandFailed("hash-object: no stdin".into()))?;
    let input: String = paths.iter().map(|p| format!("{p}\n")).collect();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = supervise(child)?;
    let _ = writer.join();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(command_failure(repo_root, &["hash-object"], &stderr));
    }
    let oids: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    if oids.len() != paths.len() {
        return Err(GitError::CommandFailed("hash-object: short answer".into()));
    }
    Ok(oids)
}

/// `git grep`'s stdout; exit 1 means "no match", which is an answer here,
/// not a failure.
fn run_grep(repo_root: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = git_command(repo_root);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = cmd.spawn().map_err(GitError::Io)?;
    let output = supervise(child)?;
    if output.status.code() == Some(1) && output.stderr.is_empty() {
        return Ok(String::new());
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(command_failure(repo_root, args, &stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// What a `git grep` found in the files the run may show, and the files it
/// found and withheld (repo-relative): an answer built on `rows` counts
/// `withheld`, since what those files say was never read (#392).
#[derive(Default)]
pub struct Grepped<T> {
    pub rows: Vec<T>,
    pub withheld: Vec<String>,
}

/// `git grep` reads every file, so its answers pass the withhold policy here,
/// at the read: a caller, a stale reference or a declaration in a withheld
/// file is as withheld as the file. Applied per surface instead, each new
/// reader of grep printed `.diffctx/ignore`d paths until someone noticed.
fn admissible<T>(
    source: &crate::source::Source,
    rows: Vec<T>,
    rel_of: impl Fn(&T) -> String,
) -> Grepped<T> {
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

/// Every line matching the extended regex `pattern` at `rev` (the working
/// tree when `None`) in the files `pathspec` names: (path, line, text).
pub fn grep_lines_matching(
    repo_root: &Path,
    source: &crate::source::Source,
    pattern: &str,
    rev: Option<&str>,
    pathspec: &[&str],
) -> Result<Grepped<(String, u32, String)>> {
    let mut args: Vec<&str> = vec!["grep", "-n", "-z", "-I", "-E", "-e", pattern];
    args.extend(rev);
    if !pathspec.is_empty() {
        args.push("--");
        args.extend_from_slice(pathspec);
    }
    grep_rows(repo_root, source, &args, rev)
}

/// `git grep -n -z` at `rev` as (repo-relative path, line, text), withheld
/// files dropped. The working tree is its tracked and unignored untracked
/// files, as every other reader of it lists them: without `--untracked` a
/// new file's own callers and tests were nowhere, and its definitions read
/// as called by nothing (#625).
fn grep_rows(
    repo_root: &Path,
    source: &crate::source::Source,
    args: &[&str],
    rev: Option<&str>,
) -> Result<Grepped<(String, u32, String)>> {
    let prefix = rev.map(|r| format!("{r}:")).unwrap_or_default();
    let mut args = args.to_vec();
    if rev.is_none() {
        args.insert(1, "--untracked");
    }
    let rows = run_grep(repo_root, &args)?
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\0');
            let path = parts.next()?;
            let path = path.strip_prefix(prefix.as_str()).unwrap_or(path);
            let no = parts.next()?.parse().ok()?;
            Some((path.to_string(), no, parts.next()?.to_string()))
        })
        .collect();
    Ok(admissible(source, rows, |(p, _, _)| p.clone()))
}

/// Tracked and unignored untracked files holding `word` as a whole word.
pub fn grep_files_with_word(
    repo_root: &Path,
    source: &crate::source::Source,
    word: &str,
    pathspec: &[String],
) -> Result<Grepped<PathBuf>> {
    grep_files(repo_root, source, &["-w", "-F", "-e", word], pathspec)
}

/// The files with a line that declares `name`: `def name`, `class name`,
/// `fn name`, `func (r T) name`, `const name =`, `name = function`.
pub fn grep_files_with_declaration(
    repo_root: &Path,
    source: &crate::source::Source,
    name: &str,
    pathspec: &[String],
) -> Result<Grepped<PathBuf>> {
    let name = regex::escape(name);
    let pattern = format!(
        r#"((def|class|fn|fun|func|function|struct|interface|trait|enum|type|typealias|module|const|let|var|val|object|define)[[:space:]]+(\([^)]*\)[[:space:]]*)?["']?{name}([^[:alnum:]_$]|$))|((^|[^[:alnum:]_$.]){name}[[:space:]]*[:=][[:space:]]*(function|async|\())"#
    );
    grep_files(repo_root, source, &["-E", "-e", &pattern], pathspec)
}

fn grep_files(
    repo_root: &Path,
    source: &crate::source::Source,
    pattern: &[&str],
    pathspec: &[String],
) -> Result<Grepped<PathBuf>> {
    let mut args: Vec<&str> = vec!["grep", "--untracked", "-l", "-z", "-I"];
    args.extend_from_slice(pattern);
    if !pathspec.is_empty() {
        validate_pathspec(pathspec)?;
        args.push("--");
        args.extend(pathspec.iter().map(String::as_str));
    }
    let rels = run_grep(repo_root, &args)?
        .split('\0')
        .filter(|p| !p.is_empty() && crate::paths::contains_lexically(Path::new(p)))
        .map(str::to_string)
        .collect();
    let found = admissible(source, rels, String::clone);
    Ok(Grepped {
        rows: found
            .rows
            .iter()
            .map(|p| crate::paths::repo_join(repo_root, p))
            .collect(),
        withheld: found.withheld,
    })
}

/// Lines of code files at `rev` that hold one of `words` as a whole word, at
/// most `per_file` per file: where a removed definition's callers are read
/// from. Unbounded, `-F -e call` matched every `callback` in every data file
/// of a monorepo and the run never stopped (#413, #457).
pub fn grep_code_words(
    repo_root: &Path,
    source: &crate::source::Source,
    words: &[String],
    rev: Option<&str>,
    per_file: usize,
) -> Result<Grepped<(String, u32, String)>> {
    if words.is_empty() {
        return Ok(Grepped::default());
    }
    let per_file = per_file.to_string();
    let globs: Vec<String> = crate::config::extensions::CODE_EXTENSIONS
        .iter()
        .map(|ext| format!(":(glob,icase)**/*{ext}"))
        .collect();
    let patterns = Patterns::new(words);
    let mut args: Vec<&str> = vec!["grep", "-n", "-z", "-I", "-F", "-w", "-m", &per_file];
    args.extend(patterns.args.iter().map(String::as_str));
    args.extend(rev);
    args.push("--");
    args.extend(globs.iter().map(String::as_str));
    grep_rows(repo_root, source, &args, rev)
}

/// The first line of each file that holds `word` as a whole word, in the
/// working tree: where a name nothing defines is spelled.
pub fn grep_word_first_lines(
    repo_root: &Path,
    source: &crate::source::Source,
    word: &str,
    pathspec: &[String],
) -> Result<Grepped<(String, u32, String)>> {
    let mut args: Vec<&str> = vec!["grep", "-n", "-z", "-I", "-F", "-w", "-m", "1", "-e", word];
    if !pathspec.is_empty() {
        validate_pathspec(pathspec)?;
        args.push("--");
        args.extend(pathspec.iter().map(String::as_str));
    }
    grep_rows(repo_root, source, &args, None)
}

/// Every line holding one of `needles` (fixed strings) at `rev`, or in the
/// working tree when `rev` is `None`: (repo-relative path, line, text).
pub fn grep_lines(
    repo_root: &Path,
    source: &crate::source::Source,
    needles: &[String],
    rev: Option<&str>,
) -> Result<Grepped<(String, u32, String)>> {
    let mut found = Grepped::default();
    // One `-F` search over a thousand needles costs more than twenty over
    // fifty: git's matcher is superlinear in the needle count (540 CPU
    // seconds for 1150 needles on a 387-file range). Chunked, a line that
    // holds needles of two chunks comes back twice and is kept once, and the
    // rows are in git's own order: by path, then line.
    for chunk in needles.chunks(GREP_NEEDLES_PER_CALL) {
        if chunk.iter().all(String::is_empty) {
            continue;
        }
        // Past the deadline the rows found are the answer, and the run says
        // it is partial.
        if !crate::resource::current_check() {
            break;
        }
        let patterns = Patterns::new(chunk);
        let mut args: Vec<&str> = vec!["grep", "-n", "-z", "-I", "-F"];
        args.extend(patterns.args.iter().map(String::as_str));
        args.extend(rev);
        let part = grep_rows(repo_root, source, &args, rev)?;
        found.rows.extend(part.rows);
        found.withheld.extend(part.withheld);
    }
    found.rows.sort_unstable();
    found.rows.dedup();
    found.withheld.sort_unstable();
    found.withheld.dedup();
    Ok(found)
}

/// Needles sent to one `git grep` by `grep_lines`.
const GREP_NEEDLES_PER_CALL: usize = 50;

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

pub fn get_untracked_files(repo_root: &Path, pathspec: &[String]) -> Result<Vec<PathBuf>> {
    let mut args: Vec<&str> = vec!["ls-files", "--others", "--exclude-standard", "-z"];
    if !pathspec.is_empty() {
        validate_pathspec(pathspec)?;
        args.push("--");
        args.extend(pathspec.iter().map(String::as_str));
    }
    let parts = run_git_z(repo_root, &args)?;
    // Lexical `root.join(p)`, never the canonical target: `canonicalize()` on a
    // symlink returns what it POINTS AT, so an untracked `evil -> /etc/shadow`
    // entered the changed-file list under an absolute out-of-root path. That
    // breaks three things at once — the path stops naming an object in the
    // repository, the ignore and secret policies are then applied to the wrong
    // name, and the working-tree line counter reads outside the checkout.
    //
    // Containment is checked lexically and NOT by canonicalising: these names
    // need not exist on disk. A deleted file is gone by definition and a bare
    // clone has no working tree, so an existence test empties both lists. The
    // symlink itself is caught where the file is actually read.
    //
    // An untracked nested repository is reported as one directory entry
    // (`vendor/nested/`, trailing slash): it is not a file of this
    // repository and has no content to fragment, so it is not a change.
    Ok(parts
        .iter()
        .filter(|p| !p.ends_with('/'))
        .filter(|p| crate::paths::contains_lexically(std::path::Path::new(p)))
        .map(|p| crate::paths::repo_join(repo_root, p))
        .collect())
}

/// Writes `content` to a fresh file in the system temp directory and returns
/// its path, or `None` if no such file could be created.
///
/// The name used to be `diffctx-ignore-<pid>-<counter>.tmp` written with
/// `fs::write`, which follows symlinks and truncates the target. Both the pid
/// and the counter are guessable, so on a shared machine anyone able to write
/// to the temp directory could pre-plant that name as a symlink and have this
/// overwrite the file it points at, with repository-controlled content.
/// `create_new` is `O_CREAT | O_EXCL`: it refuses an existing path, symlink
/// included, so a planted name costs at most a retry — and if every attempt
/// loses, the caller degrades to "no `.diffctx/ignore` patterns", which is
/// already its documented best-effort behaviour.
fn write_private_temp_file(content: &str) -> Option<PathBuf> {
    use std::io::Write;

    let dir = std::env::temp_dir();
    for _ in 0..8 {
        let unique = TEMP_EXCLUDES_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
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
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

/// `O_CREAT | O_EXCL` (`create_new`), which refuses any existing path —
/// a planted symlink included — instead of following it.
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
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn git(dir: &Path, args: &[&str]) {
        let status = git_command(dir)
            .args(args)
            .status()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(status.success(), "git {args:?} failed");
    }

    fn init_git_repo(dir: &Path) {
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
    }

    /// One `--name-status` listing answers what the three listings it
    /// replaced answered, by git's own word on each: renames, deletions and
    /// names a shell or a header would split.
    #[test]
    fn diff_names_answers_what_the_three_listings_did() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        init_git_repo(root);
        let body = "line one\nline two\nline three\nline four\n";
        let mut names = vec![
            "kept.txt",
            "moved.txt",
            "gone.txt",
            "a b.txt",
            "ünï.txt",
            "-dash.txt",
        ];
        // Windows refuses a tab in a file name.
        if cfg!(unix) {
            names.push("tab\there.txt");
        }
        for name in names {
            fs::write(root.join(name), format!("{name}\n{body}")).unwrap();
        }
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "base"]);
        fs::write(root.join("kept.txt"), "changed\n").unwrap();
        git(root, &["mv", "moved.txt", "renamed.txt"]);
        git(root, &["rm", "-q", "gone.txt", "--", "-dash.txt"]);
        fs::write(root.join("a b.txt"), "changed too\n").unwrap();
        fs::write(root.join("added.txt"), "new\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "change"]);

        let range = Some("HEAD~1..HEAD");
        let listed = |extra: &[&str]| {
            let args = diff_args(extra, range, &[]).unwrap();
            run_git(root, &args).unwrap()
        };
        let names = diff_names(root, range, &[]).unwrap();
        let joined = |s: &str| crate::paths::repo_join(root, s);

        let name_only: Vec<PathBuf> = listed(&["--name-only", "-M", "-z"])
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(joined)
            .collect();
        assert_eq!(names.changed, name_only);

        let deleted: FxHashSet<PathBuf> = listed(&["--diff-filter=D", "--name-only", "-M", "-z"])
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(joined)
            .collect();
        assert_eq!(names.deleted, deleted);
        assert_eq!(deleted.len(), 2, "{deleted:?}");

        let renames = listed(&["--diff-filter=R", "--name-status", "-M", "-z"]);
        let fields: Vec<&str> = renames.split('\0').filter(|p| !p.is_empty()).collect();
        let pairs: Vec<(String, String)> = fields
            .chunks(3)
            .map(|r| (r[1].to_string(), r[2].to_string()))
            .collect();
        assert_eq!(names.rename_pairs(), pairs);
        assert_eq!(
            pairs,
            vec![("moved.txt".to_string(), "renamed.txt".to_string())]
        );
    }

    fn commit_all(dir: &Path, message: &str) {
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", message]);
    }

    fn write_file(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&path, content).expect("write file");
    }

    // --- SAFE_DIFF_FLAGS pins the parser against hostile repo-local config ---

    struct HunkShape {
        old_start: u32,
        old_len: u32,
        new_start: u32,
        new_len: u32,
    }

    fn hunk_shapes(hunks: &[DiffHunk]) -> Vec<HunkShape> {
        hunks
            .iter()
            .map(|h| HunkShape {
                old_start: h.old_start,
                old_len: h.old_len,
                new_start: h.new_start,
                new_len: h.new_len,
            })
            .collect()
    }

    fn basenames(paths: &[PathBuf]) -> Vec<String> {
        let mut names: Vec<String> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn assert_diff_survives_hostile_config(hostile_config: &[&[&str]]) {
        let tmp = TempDir::new().expect("tempdir");
        let clean_root = tmp.path().join("clean");
        let hostile_root = tmp.path().join("hostile");
        fs::create_dir_all(&clean_root).expect("mkdir clean");
        fs::create_dir_all(&hostile_root).expect("mkdir hostile");

        let body = |a: u32, c: u32| {
            format!(
                "def f():\n    return {a}\n\n\ndef g():\n    return 0\n\n\ndef h():\n    return {c}\n"
            )
        };
        for root in [&clean_root, &hostile_root] {
            init_git_repo(root);
            write_file(root, "app.py", &body(1, 1));
            commit_all(root, "initial");
            write_file(root, "app.py", &body(2, 2));
            commit_all(root, "change");
        }
        for args in hostile_config {
            git(&hostile_root, args);
        }

        let clean_hunks =
            parse_diff(&clean_root, Some("HEAD~1..HEAD"), &[]).expect("clean parse_diff");
        let hostile_hunks =
            parse_diff(&hostile_root, Some("HEAD~1..HEAD"), &[]).expect("hostile parse_diff");
        assert!(
            !hostile_hunks.is_empty(),
            "hostile git config reduced the diff to zero hunks"
        );
        assert_eq!(
            hunk_shapes(&hostile_hunks)
                .iter()
                .map(|s| (s.old_start, s.old_len, s.new_start, s.new_len))
                .collect::<Vec<_>>(),
            hunk_shapes(&clean_hunks)
                .iter()
                .map(|s| (s.old_start, s.old_len, s.new_start, s.new_len))
                .collect::<Vec<_>>(),
            "hostile config changed the parsed hunk shape vs a clean-config repo"
        );

        let clean_files = diff_names(&clean_root, Some("HEAD~1..HEAD"), &[])
            .expect("clean changed files")
            .changed;
        let hostile_files = diff_names(&hostile_root, Some("HEAD~1..HEAD"), &[])
            .expect("hostile changed files")
            .changed;
        assert!(
            !hostile_files.is_empty(),
            "hostile git config reduced changed_files to empty"
        );
        assert_eq!(
            basenames(&hostile_files),
            basenames(&clean_files),
            "hostile config changed the changed_files set vs a clean-config repo"
        );
    }

    #[test]
    fn diff_survives_diff_noprefix() {
        assert_diff_survives_hostile_config(&[&["config", "diff.noprefix", "true"]]);
    }

    #[test]
    fn diff_survives_diff_mnemonic_prefix() {
        assert_diff_survives_hostile_config(&[&["config", "diff.mnemonicPrefix", "true"]]);
    }

    #[test]
    fn diff_survives_custom_src_dst_prefix() {
        assert_diff_survives_hostile_config(&[
            &["config", "diff.srcPrefix", "x/"],
            &["config", "diff.dstPrefix", "y/"],
        ]);
    }

    #[test]
    fn diff_survives_color_ui_always() {
        assert_diff_survives_hostile_config(&[&["config", "color.ui", "always"]]);
    }

    /// Merged across the untouched `g`, the two edits became one hunk and
    /// `g` was emitted as changed.
    #[test]
    fn diff_survives_inter_hunk_context() {
        assert_diff_survives_hostile_config(&[&["config", "diff.interHunkContext", "20"]]);
    }

    /// `GIT_DIFF_OPTS` overrides `--unified` inside git, so it must never
    /// reach a child; the end-to-end run with the variable set lives in
    /// `tests/test_git_config_resilience.py`.
    #[test]
    fn git_diff_opts_is_scrubbed_from_every_child() {
        let cmd = git_command(Path::new("."));
        assert!(
            cmd.get_envs()
                .any(|(k, v)| k == "GIT_DIFF_OPTS" && v.is_none()),
            "GIT_DIFF_OPTS is not removed from the child environment"
        );
    }

    // --- validate_diff_range: reject argv-injection ranges, keep legit ones ---

    #[test]
    fn validate_diff_range_rejects_option_smuggled_in_range() {
        for hostile in ["HEAD..--ext-diff", "a...-p", "..--upload-pack=x"] {
            assert!(
                validate_diff_range(hostile).is_err(),
                "expected {hostile:?} to be rejected"
            );
        }
    }

    #[test]
    fn validate_diff_range_accepts_legitimate_ranges() {
        for legit in [
            "HEAD~1..HEAD",
            "@{-1}..HEAD",
            "HEAD~2...origin/main",
            "main..feature/x",
            "v1.2.3+build.7",
            "release,2026..rc=1",
            "issue#42...fix%20",
        ] {
            assert!(
                validate_diff_range(legit).is_ok(),
                "expected {legit:?} to be accepted"
            );
        }
    }

    // --- duration ranges: `--diff 24h` is a window, not a revision ---

    #[test]
    fn duration_specs_cover_the_standard_units_and_compose() {
        for (spec, expected) in [
            ("5s", 5),
            ("90 sec", 90),
            ("10min", 600),
            ("45m", 2700),
            ("24h", 86_400),
            ("3hrs", 10_800),
            ("8d", 691_200),
            ("2 weeks", 1_209_600),
            ("1h30m", 5400),
            ("1D", 86_400),
        ] {
            assert_eq!(
                parse_duration_seconds(spec),
                Some(expected),
                "spec {spec:?}"
            );
        }
    }

    #[test]
    fn anything_that_is_not_wholly_a_duration_stays_a_revision() {
        for spec in [
            "HEAD",
            "HEAD~1..HEAD",
            "main",
            "8dd",
            "24",
            "h",
            "v1.2",
            "",
            "1h-",
            "deadbeef",
        ] {
            assert_eq!(parse_duration_seconds(spec), None, "spec {spec:?}");
        }
    }

    #[test]
    fn a_duration_resolves_to_the_last_commit_before_the_window() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        init_git_repo(root);
        write_file(root, "old.txt", "old\n");
        // `--before` filters on the committer date, so backdating the author
        // date alone would leave this commit inside the window.
        git(root, &["add", "-A"]);
        let status = git_command(root)
            .args(["commit", "-q", "-m", "old"])
            .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00+00:00")
            .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00+00:00")
            .status()
            .expect("commit");
        assert!(status.success());
        let old_head = run_git(root, &["rev-parse", "HEAD"])
            .expect("rev-parse")
            .trim()
            .to_string();
        write_file(root, "new.txt", "new\n");
        commit_all(root, "new");

        let base = duration_base(root, "24h").expect("resolve");
        assert_eq!(base.as_deref(), Some(old_head.as_str()));

        let diff = get_diff_text(root, base.as_deref(), &[]).expect("diff");
        assert!(diff.contains("new.txt"), "window must cover the new commit");
        assert!(
            !diff.contains("old.txt"),
            "window must exclude the commit before it"
        );
    }

    #[test]
    fn a_window_older_than_the_repo_falls_back_to_the_empty_tree() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        init_git_repo(root);
        write_file(root, "only.txt", "only\n");
        commit_all(root, "only");

        let base = duration_base(root, "1w").expect("resolve");
        assert!(base.is_some());
        let diff = get_diff_text(root, base.as_deref(), &[]).expect("diff");
        assert!(
            diff.contains("only.txt"),
            "a repo younger than the window is entirely new within it"
        );
    }

    #[test]
    fn a_ref_that_looks_like_a_duration_keeps_its_git_meaning() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        init_git_repo(root);
        write_file(root, "a.txt", "a\n");
        commit_all(root, "a");
        git(root, &["branch", "24h"]);

        assert_eq!(duration_base(root, "24h").expect("resolve"), None);
    }

    // --- unquote_c_style + the quoted diff-header branch ---

    #[test]
    fn unquote_c_style_decodes_octal_utf8_escapes() {
        // Exactly what git emits for `café.py` under the default
        // core.quotePath=true: é is UTF-8 0xC3 0xA9, i.e. octal 303 251.
        let quoted = r#""a/caf\303\251.py""#;
        assert_eq!(unquote_c_style(quoted), "a/café.py");
    }

    #[test]
    fn unquote_c_style_leaves_unquoted_input_untouched() {
        assert_eq!(unquote_c_style("a/plain.py"), "a/plain.py");
    }

    #[test]
    fn parse_path_line_takes_quoted_branch_for_old_and_new_headers() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        // The path must exist on disk: parse_path_line canonicalizes the
        // joined path to guard against traversal, and a nonexistent target
        // can fail to canonicalize while the (existing) root does, tripping
        // the containment check on platforms where the temp dir sits behind
        // a symlink (e.g. macOS /var -> /private/var) for reasons unrelated
        // to the quoted-header parsing this test targets.
        write_file(root, "café.py", "value = 1\n");

        let old_line = r#"--- "a/caf\303\251.py""#;
        let (kind, path) = parse_path_line(old_line, root);
        assert_eq!(kind, "old");
        assert_eq!(
            path.expect("old path")
                .file_name()
                .unwrap()
                .to_string_lossy(),
            "café.py"
        );

        let new_line = r#"+++ "b/caf\303\251.py""#;
        let (kind, path) = parse_path_line(new_line, root);
        assert_eq!(kind, "new");
        assert_eq!(
            path.expect("new path")
                .file_name()
                .unwrap()
                .to_string_lossy(),
            "café.py"
        );
    }

    /// The containment check is lexical (`Path::starts_with` compares
    /// components) and `canonicalize` cannot resolve a path that does not
    /// exist, so the fallback kept `..` in place and `<root>/../x` "started
    /// with" `<root>`. This only failed on macOS, where the temp root
    /// canonicalizes through `/var -> /private/var` and the two spellings stop
    /// matching — so the guard was passing for an accident of layout. Both the
    /// existing and non-existing target are covered: the first is the one that
    /// actually reads a file outside the repository.
    #[test]
    fn a_header_escaping_the_repo_root_is_refused_whether_or_not_the_target_exists() {
        let tmp = TempDir::new().expect("tempdir");
        // Canonicalized on purpose. With the root spelled the same way
        // `canonicalize` would spell it, the lexical fallback prefix matches and
        // the hole reproduces here exactly as it did on Linux CI; spelled via a
        // symlinked temp dir (`/var` on macOS) the mismatch hid it.
        let base = dunce::canonicalize(tmp.path()).expect("canonical tempdir");
        let root = base.join("repo");
        std::fs::create_dir_all(&root).expect("mkdir repo");
        std::fs::write(base.join("outside.py"), "secret = 1\n").expect("write outside");

        for rel in ["../outside.py", "../missing.py", "sub/../../outside.py"] {
            for line in [format!("--- a/{rel}"), format!("+++ b/{rel}")] {
                let (kind, path) = parse_path_line(&line, &root);
                assert_eq!(
                    (kind, path.as_ref()),
                    ("", None),
                    "escaping header accepted: {line}"
                );
            }
        }

        // An ordinary in-repo header still resolves, including one whose file
        // does not exist yet (the old side of a deletion).
        std::fs::write(root.join("real.py"), "x = 1\n").expect("write real");
        for rel in ["real.py", "gone.py", "nested/deep.py"] {
            let (kind, path) = parse_path_line(&format!("--- a/{rel}"), &root);
            assert_eq!(kind, "old", "in-repo header refused: {rel}");
            assert!(path.expect("path").ends_with(rel));
        }
    }

    /// A symlink inside the repository needs no `..` to point outside it, so
    /// rejecting `..` is not on its own enough — the canonical containment check
    /// has to stay reachable rather than being short-circuited by a lexical
    /// fallback that is always true once `..` is gone.
    #[cfg(unix)]
    #[test]
    fn an_in_repo_symlink_pointing_outside_the_root_is_refused() {
        let tmp = TempDir::new().expect("tempdir");
        let base = dunce::canonicalize(tmp.path()).expect("canonical tempdir");
        let root = base.join("repo");
        std::fs::create_dir_all(&root).expect("mkdir repo");

        let outside_dir = base.join("outside");
        std::fs::create_dir_all(&outside_dir).expect("mkdir outside");
        std::fs::write(outside_dir.join("secret.py"), "token = 1\n").expect("write secret");
        std::os::unix::fs::symlink(&outside_dir, root.join("escape"))
            .expect("symlink into the repo");

        let (kind, path) = parse_path_line("--- a/escape/secret.py", &root);
        assert_eq!(
            (kind, path.as_ref()),
            ("", None),
            "a header reaching outside the repo through an in-repo symlink was accepted"
        );

        // A symlink that stays inside the repository is still fine.
        std::fs::create_dir_all(root.join("real")).expect("mkdir real");
        std::fs::write(root.join("real/mod.py"), "y = 1\n").expect("write real");
        std::os::unix::fs::symlink(root.join("real"), root.join("alias")).expect("inner symlink");
        let (kind, _) = parse_path_line("--- a/alias/mod.py", &root);
        assert_eq!(kind, "old", "an in-repo symlink was wrongly refused");
    }

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

    /// The traversal guard in `parse_path_line` returns `("", None)`, which the
    /// hunk loop cannot distinguish from "this line is not a path header". With
    /// no per-file reset the previous file's paths stay live, so the rejected
    /// entry's hunks are charged to the last legitimate file — the guard drops
    /// the path but keeps its line ranges, and those ranges decide which
    /// fragments are marked as changed.
    #[test]
    fn a_rejected_path_header_does_not_charge_its_hunks_to_the_previous_file() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        write_file(root, "real.py", "a = 1\nb = 2\nc = 3\n");

        let output = concat!(
            "diff --git a/real.py b/real.py\n",
            "--- a/real.py\n",
            "+++ b/real.py\n",
            "@@ -1,1 +1,1 @@\n",
            "-a = 1\n",
            "+a = 9\n",
            "diff --git a/../../escape.py b/../../escape.py\n",
            "--- a/../../escape.py\n",
            "+++ b/../../escape.py\n",
            "@@ -500,20 +500,20 @@\n",
            "-gone\n",
            "+new\n",
        );

        let hunks = parse_hunks_from_diff_output(output, root);
        assert_eq!(
            hunks.len(),
            1,
            "expected only the in-repo file's hunk, got {:?}",
            hunks
                .iter()
                .map(|h| (h.path.as_ref().to_string(), h.new_start))
                .collect::<Vec<_>>()
        );
        assert_eq!(hunks[0].new_start, 1);
        assert!(hunks[0].path.ends_with("real.py"));
    }

    /// A deletion emits `+++ /dev/null`, and a creation emits `--- /dev/null`;
    /// both must still resolve to the side that names a real file.
    #[test]
    fn deletions_and_creations_attribute_their_hunks_to_the_named_side() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        write_file(root, "kept.py", "x = 1\n");
        write_file(root, "added.py", "y = 1\n");
        write_file(root, "removed.py", "z = 1\n");

        let output = concat!(
            "diff --git a/kept.py b/kept.py\n",
            "--- a/kept.py\n",
            "+++ b/kept.py\n",
            "@@ -1,1 +1,1 @@\n",
            "diff --git a/removed.py b/removed.py\n",
            "--- a/removed.py\n",
            "+++ /dev/null\n",
            "@@ -1,1 +0,0 @@\n",
            "diff --git a/added.py b/added.py\n",
            "--- /dev/null\n",
            "+++ b/added.py\n",
            "@@ -0,0 +1,1 @@\n",
        );

        let paths: Vec<String> = parse_hunks_from_diff_output(output, root)
            .iter()
            .map(|h| {
                Path::new(h.path.as_ref())
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(paths, vec!["kept.py", "removed.py", "added.py"]);
    }

    #[test]
    fn parse_diff_handles_real_repo_with_default_quoted_unicode_filename() {
        // core.quotePath defaults to true, so a real git diff over a renamed
        // non-ASCII file exercises the quoted branch end-to-end, not just the
        // helper in isolation.
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        init_git_repo(root);
        write_file(root, "café.py", "value = 1\n");
        commit_all(root, "initial");
        write_file(root, "café.py", "value = 2\n");
        commit_all(root, "change");

        let hunks = parse_diff(root, Some("HEAD~1..HEAD"), &[]).expect("parse_diff");
        assert!(
            !hunks.is_empty(),
            "quoted unicode diff header was not parsed into any hunk"
        );
        assert!(
            hunks.iter().any(|h| h.path.contains("café")),
            "no hunk carried the decoded unicode path, got: {:?}",
            hunks.iter().map(|h| h.path.as_ref()).collect::<Vec<_>>()
        );
    }

    // --- subprocess timeout/kill: wait_with_timeout must not hang or orphan ---

    #[test]
    fn wait_with_timeout_kills_long_running_child_and_returns_promptly() {
        let child = Command::new("sleep")
            .arg("30")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();

        let start = std::time::Instant::now();
        let result = wait_with_timeout(child, Duration::from_millis(200), &["sleep", "30"]);
        let elapsed = start.elapsed();

        assert!(
            matches!(result, Err(GitError::Timeout(_))),
            "expected Timeout error, got {result:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "wait_with_timeout should return promptly, took {elapsed:?}"
        );

        // The child must actually be reaped, not orphaned: `kill -0` on a
        // reaped pid fails once the OS releases it. Retry briefly since the
        // OS may hold a zombie slot for a moment after the kill.
        let mut still_alive = true;
        for _ in 0..20 {
            let status = Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("spawn kill -0");
            if !status.success() {
                still_alive = false;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!still_alive, "child pid {pid} was not reaped after timeout");
    }

    #[test]
    fn a_zero_ceiling_times_out_even_when_the_child_has_already_exited() {
        let child = Command::new("true")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn true");
        std::thread::sleep(Duration::from_millis(100));
        let result = wait_with_timeout(child, Duration::ZERO, &["true"]);
        assert!(
            matches!(result, Err(GitError::Timeout(_))),
            "a zero ceiling must not depend on who finishes first, got {result:?}"
        );
    }

    #[test]
    fn wait_with_timeout_does_not_penalize_fast_commands() {
        let child = Command::new("true")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn true");
        let result = wait_with_timeout(child, Duration::from_secs(5), &["true"]);
        assert!(matches!(result, Ok(ref out) if out.status.success()));
    }
}
