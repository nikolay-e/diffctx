//! The one way this crate runs git: supervised children with a deadline and a
//! cancellation, a persistent `cat-file --batch`. Ported from the facade's
//! `git.rs`; the thread-local run limits became an explicit [`Exec`] carried by
//! whoever asks, so a reader used from a worker thread keeps its run's limits.

use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use wait_timeout::ChildExt;

use crate::snapshot::error::{Result, SnapshotError};
use crate::snapshot::loss::SideTag;

pub const DEFAULT_TIMEOUT_SECONDS: u64 = 60;
const CATFILE_TERMINATION_SECONDS: u64 = 2;
/// The exit status of every git `fatal:`: a usage error, a missing revision, an
/// unreadable config, no repository. One status for all of them, so it says
/// that git refused, never which question it refused.
pub const FATAL_STATUS: i32 = 128;

thread_local! {
    static GIT_TIMEOUT_SECS: Cell<u64> = const { Cell::new(DEFAULT_TIMEOUT_SECONDS) };
}

/// The per-call ceiling of every git child started from this thread after the
/// call.
///
/// Thread-local, not a global: overlapping runs on worker threads must
/// not shorten each other's git calls (#210). A reader captures the value when
/// it is built.
pub fn set_git_timeout(secs: u64) {
    GIT_TIMEOUT_SECS.with(|c| c.set(secs));
}

thread_local! {
    static AMBIENT: RefCell<RunBudget> = const {
        RefCell::new(RunBudget { deadline: None, cancel: None, read_fault: None })
    };
}

/// Publishes a run's limits to this thread until the guard drops: every git
/// child the engine's questions start here ([`run_git`](crate::snapshot::run_git)
/// and its kin) is bounded by them, as a reader is by the budget it was built with.
pub fn enter_run(budget: RunBudget) -> RunScope {
    RunScope(AMBIENT.with(|a| a.replace(budget)))
}

/// Restores the limits of the run that was current before [`enter_run`].
pub struct RunScope(RunBudget);

impl Drop for RunScope {
    fn drop(&mut self) {
        let before = std::mem::take(&mut self.0);
        AMBIENT.with(|a| *a.borrow_mut() = before);
    }
}

/// What a run lets a git child spend, beyond the per-call ceiling: the run's
/// own deadline and its cancellation.
#[derive(Clone, Debug, Default)]
pub struct RunBudget {
    pub deadline: Option<Instant>,
    pub cancel: Option<Arc<AtomicBool>>,
    /// Test only: fails one blob read of the run on purpose.
    pub read_fault: Option<Arc<ReadFault>>,
}

/// Fails the `at`-th blob read of a run, on purpose.
///
/// Counted from 0 over every reader of the run, base and head alike, or over the
/// reads of one side when `side` says which (`base:3`), and failed with an error
/// that says so. Test only: it lets a sweep fail each read of a run in turn, the
/// way the git sweeps fail each call, and strike the base side alone.
#[derive(Debug)]
pub struct ReadFault {
    at: usize,
    side: Option<SideTag>,
    seen: std::sync::atomic::AtomicUsize,
}

impl ReadFault {
    pub fn at(at: usize) -> Arc<Self> {
        Self::scoped(None, at)
    }

    pub fn scoped(side: Option<SideTag>, at: usize) -> Arc<Self> {
        Arc::new(Self {
            at,
            side,
            seen: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// `N`, `base:N` or `head:N`.
    pub fn parse(spec: &str) -> Option<Arc<Self>> {
        let (side, at) = match spec.split_once(':') {
            Some(("base", n)) => (Some(SideTag::Base), n),
            Some(("head", n)) => (Some(SideTag::Head), n),
            Some(_) => return None,
            None => (None, spec),
        };
        // A malformed knob names no read: there is no fault.
        match at.parse() {
            Ok(at) => Some(Self::scoped(side, at)),
            Err(_) => None,
        }
    }

    fn strikes(&self, reading: SideTag) -> bool {
        if self.side.is_some_and(|only| only != reading) {
            return false;
        }
        let n = self.seen.fetch_add(1, Ordering::Relaxed);
        if n == self.at {
            eprintln!("diffctx: injected read fault: blob read {n} failed on purpose");
        }
        n == self.at
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Exec {
    timeout_secs: u64,
    budget: RunBudget,
}

/// The slice a blocked wait sleeps before it looks at the cancellation flag
/// again; a finished child is noticed at once whatever the slice.
const POLL_SLICE: Duration = Duration::from_millis(100);
/// How long reaping a killed child may take before it is given up on.
const REAP_GRACE: Duration = Duration::from_secs(2);
const MAX_STDERR_BYTES: usize = 64 * 1024;
const MAX_BATCH_HEADER_BYTES: u64 = 4096;

struct Limits {
    deadline: Instant,
    cancel: Option<Arc<AtomicBool>>,
    timeout_secs: u64,
}

impl Limits {
    fn exhausted(&self) -> Option<SnapshotError> {
        if self
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
        {
            Some(SnapshotError::Cancelled)
        } else if Instant::now() >= self.deadline {
            Some(SnapshotError::Timeout(self.timeout_secs))
        } else {
            None
        }
    }

    fn next_slice(&self) -> Duration {
        self.deadline
            .saturating_duration_since(Instant::now())
            .min(POLL_SLICE)
    }
}

impl Exec {
    pub(crate) fn ambient() -> Self {
        Self::with_budget(RunBudget::default())
    }

    /// Bounded by the limits of the run on this thread, none outside a run.
    pub(crate) fn current() -> Self {
        Self::with_budget(AMBIENT.with(|a| a.borrow().clone()))
    }

    pub(crate) fn with_budget(budget: RunBudget) -> Self {
        Self {
            timeout_secs: GIT_TIMEOUT_SECS.with(Cell::get),
            budget,
        }
    }

    fn limits(&self) -> Limits {
        let ceiling = Instant::now() + Duration::from_secs(self.timeout_secs);
        Limits {
            deadline: self.budget.deadline.map_or(ceiling, |d| d.min(ceiling)),
            cancel: self.budget.cancel.clone(),
            timeout_secs: self.timeout_secs,
        }
    }

    /// The injected fault of a test run, when this read is the one it names.
    pub(crate) fn inject_read_fault(&self, reading: SideTag) -> Result<()> {
        match &self.budget.read_fault {
            Some(fault) if fault.strikes(reading) => Err(SnapshotError::Git(
                "injected read fault (DIFFCTX_TEST_READ_FAULT)".into(),
            )),
            _ => Ok(()),
        }
    }

    /// `Err` at once when the run is already over, before any child starts.
    pub(crate) fn check(&self) -> Result<()> {
        self.limits().exhausted().map_or(Ok(()), Err)
    }

    /// Runs a prepared git command (from [`git_command`]) to completion under
    /// the run's limits. A non-zero exit is an error unless its code is in
    /// `accept`; `label` names the call in the message.
    pub(crate) fn finish(&self, cmd: Command, label: &[&str], accept: &[i32]) -> Result<Output> {
        let output = self.output(cmd)?;
        let ok = output.status.success()
            || output
                .status
                .code()
                .is_some_and(|code| accept.contains(&code));
        if ok {
            Ok(output)
        } else {
            Err(refusal(label, &output))
        }
    }

    /// A prepared command run to completion under the run's limits, whatever
    /// its exit status: the caller reads the status.
    pub(crate) fn output(&self, mut cmd: Command) -> Result<Output> {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SnapshotError::Git("git is not installed or not in PATH".into())
            } else {
                SnapshotError::Io(e)
            }
        })?;
        supervise(child, &self.limits())
    }

    /// `finish` for a command that reads `input` on its stdin.
    pub(crate) fn feed(&self, mut cmd: Command, label: &[&str], input: Vec<u8>) -> Result<Output> {
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(SnapshotError::Io)?;
        let mut stdin = child.stdin.take().ok_or_else(|| {
            SnapshotError::Git(format!("git {}: no stdin", label.first().unwrap_or(&"")))
        })?;
        let writer = std::thread::spawn(move || stdin.write_all(&input));
        let output = supervise(child, &self.limits())?;
        let _ = writer.join();
        if output.status.success() {
            Ok(output)
        } else {
            Err(refusal(label, &output))
        }
    }

    pub(crate) fn bytes(&self, root: &Path, args: &[&str]) -> Result<Vec<u8>> {
        let mut cmd = git_command(root);
        cmd.args(args);
        self.finish(cmd, args, &[]).map(|out| out.stdout)
    }

    pub(crate) fn text(&self, root: &Path, args: &[&str]) -> Result<String> {
        self.bytes(root, args)
            .map(|out| String::from_utf8_lossy(&out).into_owned())
    }

    /// NUL-separated records, raw: a path need not be UTF-8.
    pub(crate) fn records(&self, root: &Path, args: &[&str]) -> Result<Vec<Vec<u8>>> {
        Ok(self
            .bytes(root, args)?
            .split(|b| *b == 0)
            .filter(|record| !record.is_empty())
            .map(<[u8]>::to_vec)
            .collect())
    }

    /// The one situation in which an unknown revision is not a typo but a
    /// missing history. A failed probe is "no": it only decorates a message.
    pub(crate) fn is_shallow(&self, root: &Path) -> bool {
        self.text(root, &["rev-parse", "--is-shallow-repository"])
            .is_ok_and(|out| out.trim() == "true")
    }
}

/// The error of a git child that exited non-zero: its status, and its own
/// line for a person to read.
pub(crate) fn refusal(args: &[&str], output: &Output) -> SnapshotError {
    let stderr = String::from_utf8_lossy(&output.stderr);
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
        .unwrap_or("unknown error")
        .to_string();
    SnapshotError::GitRefused {
        subcommand: subcommand.to_string(),
        status: output.status.code(),
        reason,
    }
}

/// Always targets the repo via `-C`.
///
/// The variables that tell git which repository, work tree, index or object
/// store to use. A git hook exports them; a child that inherits them reads
/// and writes the hook's repository instead of the one it was pointed at.
pub const REPO_LOCATING_ENV: [&str; 7] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
];

/// The one way the crate builds a git command, the facade's included. The
/// repo-locating variables inherited from a parent process are scrubbed: they
/// silently redirect every command to the wrong repository.
#[allow(
    clippy::disallowed_methods,
    reason = "this module is the one door that starts git"
)]
pub fn git_command(root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        // Under the MCP server stdin is the JSON-RPC pipe the client holds
        // open; a child inheriting it never exited on Windows.
        .stdin(Stdio::null())
        // A partial clone lazily fetches blobs, and a fetch that wants a
        // credential would block on the terminal for the whole timeout.
        .env("GIT_TERMINAL_PROMPT", "0")
        // git translates its messages and its `fatal:` prefix: a person reads
        // them, nothing here branches on them, and a child run in the C locale
        // answers the same way on every machine.
        .env("LC_ALL", "C")
        .env_remove("LANGUAGE")
        // `GIT_DIFF_OPTS` overrides `--unified`: a `-u3` in a user's shell
        // changed which fragment counted as the core (#263).
        .env_remove("GIT_DIFF_OPTS");
    for name in REPO_LOCATING_ENV {
        cmd.env_remove(name);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd
}

/// Kills the child and everything it started: on Unix every git child leads
/// its own process group, so a descendant holding the stdout pipe open dies
/// with it instead of keeping a reader blocked.
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

type PipeRead = mpsc::Receiver<std::io::Result<Vec<u8>>>;

fn spawn_pipe_reader(mut pipe: impl Read + Send + 'static, keep: usize) -> PipeRead {
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

fn await_pipe(rx: Option<PipeRead>, limits: &Limits) -> Result<Vec<u8>> {
    let Some(rx) = rx else {
        return Ok(Vec::new());
    };
    loop {
        if let Some(stop) = limits.exhausted() {
            return Err(stop);
        }
        match rx.recv_timeout(limits.next_slice()) {
            Ok(read) => return read.map_err(SnapshotError::Io),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            // A reader that vanished used to read as an empty buffer: an
            // unreadable `git diff` then looked exactly like an empty diff.
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(SnapshotError::Io(std::io::Error::other(
                    "git pipe reader thread panicked",
                )));
            }
        }
    }
}

/// Waits for the child, its stdout and its stderr together under one deadline.
/// The pipes are read on their own threads and awaited through channels, so
/// neither a stalled child nor a descendant that inherited a pipe can hold the
/// caller past the deadline.
fn supervise(mut child: Child, limits: &Limits) -> Result<Output> {
    let stdout = child
        .stdout
        .take()
        .map(|s| spawn_pipe_reader(s, usize::MAX));
    let stderr = child
        .stderr
        .take()
        .map(|s| spawn_pipe_reader(s, MAX_STDERR_BYTES));
    // A spent budget is judged before the child is asked: on a fast machine
    // `wait_timeout(0)` reaps an already-finished git and reports success.
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
        await_pipe(stderr, limits).map(|stderr| Output {
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

/// A private, exclusively created scratch directory, removed when dropped.
/// Created with `create_dir`, which refuses an existing path: a pre-planted
/// name (a symlink, a directory with a hostile config) costs a retry, never
/// an adoption.
pub(crate) struct Scratch(PathBuf);

impl Scratch {
    pub(crate) fn new() -> Result<Self> {
        use std::sync::atomic::AtomicU64;
        static SEQ: AtomicU64 = AtomicU64::new(0);
        for _ in 0..16 {
            let dir = std::env::temp_dir().join(format!(
                "diffctx-snapshot-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            match create_private_dir(&dir) {
                Ok(()) => return Ok(Self(dir)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(SnapshotError::Io(e)),
            }
        }
        Err(SnapshotError::Io(std::io::Error::other(
            "no free scratch directory name",
        )))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir(path)
}

/// One `git cat-file --batch` answer as the reader of the connection saw it.
pub(crate) enum Reply {
    Blob(Vec<u8>),
    /// git has no such object. The caller knows whether the tree it listed
    /// names one: that is a broken repository, not an absent file.
    Missing,
    /// An object that is not a blob (a tree, a commit).
    NotBlob,
    TooLarge(usize),
}

enum Raw {
    Reply(Reply),
    /// The stream can no longer be trusted: malformed framing, early EOF, an
    /// I/O error. The connection is discarded.
    Broken(String),
}

/// A live `cat-file --batch` child. The caller owns the `Child` and nothing
/// else touches it, so killing it never waits on a lock held by a blocked
/// read; the pipes belong to a worker thread the caller only ever awaits with
/// a deadline.
struct BatchConn {
    child: Child,
    requests: mpsc::SyncSender<Vec<u8>>,
    replies: mpsc::Receiver<Raw>,
}

impl BatchConn {
    fn start(root: &Path, max_bytes: usize) -> Result<Self> {
        let mut child = git_command(root)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            kill_tree(&mut child);
            return Err(SnapshotError::Protocol(
                "failed to capture the batch pipes".into(),
            ));
        };
        let (requests, request_rx) = mpsc::sync_channel::<Vec<u8>>(1);
        let (reply_tx, replies) = mpsc::sync_channel(1);
        std::thread::spawn(move || batch_worker(stdin, stdout, &request_rx, &reply_tx, max_bytes));
        Ok(Self {
            child,
            requests,
            replies,
        })
    }

    /// A graceful close gives git the cleanup allowance to exit on EOF, cut to
    /// what the run has left; a cancelled or expired run kills at once.
    fn shut_down(mut self, graceful: Option<&Limits>) {
        drop(self.requests);
        if let Some(limits) = graceful {
            let allowance = Duration::from_secs(CATFILE_TERMINATION_SECONDS)
                .min(limits.deadline.saturating_duration_since(Instant::now()));
            if limits.exhausted().is_none()
                && matches!(self.child.wait_timeout(allowance), Ok(Some(_)))
            {
                return;
            }
        }
        kill_tree(&mut self.child);
    }
}

fn batch_worker(
    mut stdin: ChildStdin,
    stdout: ChildStdout,
    requests: &mpsc::Receiver<Vec<u8>>,
    replies: &mpsc::SyncSender<Raw>,
    max_bytes: usize,
) {
    let mut reader = BufReader::new(stdout);
    while let Ok(request) = requests.recv() {
        let reply = match stdin.write_all(&request).and_then(|()| stdin.flush()) {
            Ok(()) => read_batch_reply(&mut reader, max_bytes),
            Err(e) => Raw::Broken(format!("write failed: {e}")),
        };
        let broken = matches!(reply, Raw::Broken(_));
        if replies.send(reply).is_err() || broken {
            return;
        }
    }
}

fn read_batch_reply(reader: &mut BufReader<ChildStdout>, max_bytes: usize) -> Raw {
    let mut header = Vec::new();
    match reader
        .by_ref()
        .take(MAX_BATCH_HEADER_BYTES)
        .read_until(b'\n', &mut header)
    {
        Ok(0) => return Raw::Broken("unexpected EOF".into()),
        Ok(_) if header.last() != Some(&b'\n') => {
            return Raw::Broken("unterminated header".into());
        }
        Ok(_) => {}
        Err(e) => return Raw::Broken(format!("read failed: {e}")),
    }
    let header = String::from_utf8_lossy(&header);
    let header = header.trim_end();
    if header.ends_with(" missing") || header.ends_with(" ambiguous") {
        return Raw::Reply(Reply::Missing);
    }
    let mut parts = header.rsplitn(3, ' ');
    let (Some(size), Some(kind)) = (parts.next(), parts.next()) else {
        return Raw::Broken(format!("malformed header: {header}"));
    };
    let Ok(size) = size.parse::<usize>() else {
        return Raw::Broken(format!("invalid size in header: {header}"));
    };
    let keep = kind == "blob" && size <= max_bytes;
    // An object that is not kept is drained in bounded chunks, so the stream
    // stays in step for the next request without allocating `size` bytes.
    let mut body = if keep { vec![0u8; size] } else { Vec::new() };
    let read = if keep {
        reader.read_exact(&mut body)
    } else {
        std::io::copy(&mut reader.by_ref().take(size as u64), &mut std::io::sink()).and_then(|n| {
            if n == size as u64 {
                Ok(())
            } else {
                Err(std::io::ErrorKind::UnexpectedEof.into())
            }
        })
    };
    // The record terminator is part of the protocol: failing to consume it
    // leaves the stream one byte off for every request after this one.
    let mut terminator = [0u8; 1];
    if let Err(e) = read.and_then(|()| reader.read_exact(&mut terminator)) {
        return Raw::Broken(format!("body read failed: {e}"));
    }
    if terminator[0] != b'\n' {
        return Raw::Broken("record terminator missing".into());
    }
    Raw::Reply(match (keep, kind) {
        (true, _) => Reply::Blob(body),
        (false, "blob") => Reply::TooLarge(size),
        (false, _) => Reply::NotBlob,
    })
}

pub(crate) struct CatBatch {
    root: PathBuf,
    max_bytes: usize,
    exec: Exec,
    conn: Option<BatchConn>,
}

impl CatBatch {
    /// The child starts on the first request: a reader of the working tree
    /// alone never spawns one.
    pub(crate) fn new(root: &Path, max_bytes: usize, exec: Exec) -> Self {
        Self {
            root: root.to_path_buf(),
            max_bytes,
            exec,
            conn: None,
        }
    }

    /// The object `oid` names. A connection that went wrong is replaced once
    /// and the request asked again; a second failure is an error, never an
    /// absent object.
    pub(crate) fn get(&mut self, oid: &str) -> Result<Reply> {
        let mut last = String::new();
        for _ in 0..2 {
            match self.ask(oid)? {
                Raw::Reply(reply) => return Ok(reply),
                Raw::Broken(why) => last = why,
            }
        }
        Err(SnapshotError::Protocol(format!("{last} ({oid})")))
    }

    fn ask(&mut self, oid: &str) -> Result<Raw> {
        let limits = self.exec.limits();
        if let Some(stop) = limits.exhausted() {
            return Err(stop);
        }
        let conn = match self.conn.take() {
            Some(conn) => conn,
            None => BatchConn::start(&self.root, self.max_bytes)?,
        };
        if conn.requests.send(format!("{oid}\n").into_bytes()).is_err() {
            conn.shut_down(None);
            return Ok(Raw::Broken(format!("connection closed before {oid}")));
        }
        let reply = loop {
            if let Some(stop) = limits.exhausted() {
                // A half-consumed stream is never reused: the next request
                // would read this one's leftover bytes as its own answer.
                conn.shut_down(None);
                return Err(stop);
            }
            match conn.replies.recv_timeout(limits.next_slice()) {
                Ok(reply) => break reply,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break Raw::Broken("worker exited".into());
                }
            }
        };
        match reply {
            Raw::Broken(why) => {
                conn.shut_down(None);
                Ok(Raw::Broken(why))
            }
            reply @ Raw::Reply(_) => {
                self.conn = Some(conn);
                Ok(reply)
            }
        }
    }

    pub(crate) fn close(&mut self) {
        let limits = self.exec.limits();
        if let Some(conn) = self.conn.take() {
            conn.shut_down(Some(&limits));
        }
    }
}

impl Drop for CatBatch {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test starts the child it supervises"
)]
mod tests {
    use super::*;

    fn within(timeout: Duration) -> Limits {
        Limits {
            deadline: Instant::now() + timeout,
            cancel: None,
            timeout_secs: 0,
        }
    }

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
        let result = supervise(child, &within(Duration::from_millis(200)));
        let elapsed = start.elapsed();

        assert!(
            matches!(result, Err(SnapshotError::Timeout(_))),
            "expected Timeout error, got {result:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "supervise should return promptly, took {elapsed:?}"
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
        let result = supervise(child, &within(Duration::ZERO));
        assert!(
            matches!(result, Err(SnapshotError::Timeout(_))),
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
        let result = supervise(child, &within(Duration::from_secs(5)));
        assert!(matches!(result, Ok(ref out) if out.status.success()));
    }
}
