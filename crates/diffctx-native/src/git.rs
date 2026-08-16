use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::FxHashSet;
use wait_timeout::ChildExt;

use crate::config::git::{self, GIT};
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
}

// PID alone is not unique within a process: the MCP server runs each tool
// body on its own worker thread, so two overlapping pipelines can both reach
// `find_ignored_paths_with_source` under the same PID. A shared filename means whoever
// finishes first deletes the other's still-in-use excludesFile; git tolerates
// a missing `core.excludesFile` silently, so the loser's `.diffctx/ignore`
// rules are dropped without error. The counter makes every call's temp path
// unique regardless of thread interleaving.
static TEMP_EXCLUDES_COUNTER: AtomicU64 = AtomicU64::new(0);

fn git_timeout() -> u64 {
    GIT_TIMEOUT_SECS.with(|c| c.get())
}
// The prefix and color flags are not cosmetic: the diff parser keys off the
// literal `--- a/` / `+++ b/` headers, so a user's `diff.noprefix`,
// `diff.mnemonicPrefix`, `diff.srcPrefix`/`dstPrefix` or `color.ui=always`
// silently reduced every run to zero fragments and an empty `changed_files`.
const SAFE_DIFF_FLAGS: &[&str] = &[
    "--no-textconv",
    "--no-ext-diff",
    "--no-color",
    "--src-prefix=a/",
    "--dst-prefix=b/",
];

static HUNK_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@").unwrap());

static RANGE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s*(\S+?)(\.\.\.?)(\S*?)\s*$").unwrap());

// Neither side of a range may begin with `-`: a leading dash would be parsed
// by git as an option rather than a revision, so a caller-supplied range like
// `--ext-diff` or `--textconv` would re-enable the very filters SAFE_DIFF_FLAGS
// disables and run repo-configured commands. Refs that begin with a dash are
// unaddressable on a git command line anyway, so nothing legitimate is lost.
static SAFE_RANGE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"^[a-zA-Z0-9_.^~/@{}][a-zA-Z0-9_.^~/@{}\-]*(\.\.\.?([a-zA-Z0-9_.^~/@{}][a-zA-Z0-9_.^~/@{}\-]*)?)?$",
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
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("timeout after {0}s")]
    Timeout(u64),
}

pub type Result<T> = std::result::Result<T, GitError>;

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

pub struct ResolvedRange {
    pub range: Option<String>,
    /// A duration resolves against the live working tree, so untracked files
    /// belong in the change set exactly as they do for a bare `--diff`.
    pub from_duration: bool,
}

impl ResolvedRange {
    fn verbatim(diff_range: Option<&str>) -> Self {
        Self {
            range: diff_range.map(str::to_string),
            from_duration: false,
        }
    }
}

/// The oid of the empty tree, used as the base when the repository is younger
/// than the requested window: every file is then genuinely new within it.
/// Derived from the repo's hash algorithm rather than hardcoded to sha1.
fn empty_tree_oid(repo_root: &Path) -> String {
    const SHA1_EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904"; // pragma: allowlist secret
    const SHA256_EMPTY_TREE: &str =
        "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321"; // pragma: allowlist secret
    match run_git(repo_root, &["rev-parse", "--show-object-format"]) {
        Ok(format) if format.trim() == "sha256" => SHA256_EMPTY_TREE.to_string(),
        _ => SHA1_EMPTY_TREE.to_string(),
    }
}

fn rev_exists(repo_root: &Path, rev: &str) -> bool {
    if validate_rev(rev).is_err() {
        return false;
    }
    let spec = format!("{rev}^{{commit}}");
    run_git(repo_root, &["rev-parse", "--verify", "--quiet", &spec]).is_ok()
}

/// Resolves a duration spec to the last commit before the window.
///
/// Every other range is left untouched. `git diff <that commit>` covers both
/// the commits made inside the window and the uncommitted work on top.
///
/// A ref that happens to look like a duration (a branch `24h`, an abbreviated
/// sha `24d`) keeps its git meaning: the revision is probed first, so no
/// existing invocation changes behaviour.
pub fn resolve_duration_range(repo_root: &Path, diff_range: Option<&str>) -> Result<ResolvedRange> {
    let Some(spec) = diff_range else {
        return Ok(ResolvedRange::verbatim(None));
    };
    let trimmed = spec.trim();
    let Some(seconds) = parse_duration_seconds(trimmed) else {
        return Ok(ResolvedRange::verbatim(diff_range));
    };
    if rev_exists(repo_root, trimmed) {
        return Ok(ResolvedRange::verbatim(diff_range));
    }
    // git owns the calendar arithmetic (local timezone, DST) via approxidate.
    let before = format!("--before={seconds} seconds ago");
    let base = run_git(repo_root, &["rev-list", "-1", &before, "HEAD", "--"])
        .map(|out| out.trim().to_string())
        .unwrap_or_default();
    let base = if base.is_empty() {
        empty_tree_oid(repo_root)
    } else {
        base
    };
    Ok(ResolvedRange {
        range: Some(base),
        from_duration: true,
    })
}

/// Always targets the repo via `-C`.
///
/// Repo-locating variables inherited from a parent process (e.g. a git
/// hook exporting `GIT_DIR` / `GIT_INDEX_FILE`) must be scrubbed or they
/// silently redirect every command to the wrong repository.
pub fn git_command(repo_root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo_root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    cmd
}

pub fn run_git(repo_root: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = git_command(repo_root);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());

    let child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            GitError::CommandFailed("git is not installed or not in PATH".into())
        } else {
            GitError::Io(e)
        }
    })?;

    let output = wait_with_timeout(child, Duration::from_secs(git_timeout()), args)?;

    if !output.status.success() {
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
            .unwrap_or("unknown error");
        return Err(GitError::CommandFailed(format!(
            "git {subcommand} failed: {reason}"
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn wait_with_timeout(
    child: Child,
    timeout: Duration,
    _args: &[&str],
) -> Result<std::process::Output> {
    let mut child = child;
    let stdout_handle = child.stdout.take().map(|mut s| {
        std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
            let mut buf = Vec::new();
            s.read_to_end(&mut buf)?;
            Ok(buf)
        })
    });
    let stderr_handle = child.stderr.take().map(|mut s| {
        std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
            let mut buf = Vec::new();
            s.read_to_end(&mut buf)?;
            Ok(buf)
        })
    });

    let status = match child.wait_timeout(timeout)? {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(GitError::Timeout(timeout.as_secs()));
        }
    };

    let stdout = stdout_handle
        .and_then(|h| h.join().ok())
        .and_then(|r| r.ok())
        .unwrap_or_default();
    let stderr = stderr_handle
        .and_then(|h| h.join().ok())
        .and_then(|r| r.ok())
        .unwrap_or_default();

    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

pub fn is_git_repo(path: &Path) -> bool {
    run_git(path, &["rev-parse", "--git-dir"]).is_ok()
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
    Some(PathBuf::from(trimmed))
}

pub fn get_diff_text(repo_root: &Path, diff_range: Option<&str>) -> Result<String> {
    let mut args: Vec<&str> = vec!["diff"];
    args.extend_from_slice(SAFE_DIFF_FLAGS);
    if let Some(range) = diff_range {
        validate_diff_range(range)?;
        args.push(range);
    }
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
/// reason. Absolute paths are refused for the same reason — `Path::join`
/// with an absolute argument discards the root entirely.
pub(crate) fn resolve_in_repo(repo_root: &Path, rel_path: &str) -> Option<PathBuf> {
    let rel = Path::new(rel_path);
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }

    let joined = repo_root.join(rel);
    // Compare like for like. Falling back to the lexical spelling as an
    // *alternative* to the canonical check (rather than only when
    // canonicalization is impossible) would make the canonical check dead: with
    // `..` already excluded, `joined` always starts with `repo_root`, so an
    // in-repo symlink pointing outside the tree would resolve outside and still
    // be accepted.
    match joined.canonicalize() {
        Ok(resolved) => {
            let resolved_root = repo_root
                .canonicalize()
                .unwrap_or_else(|_| repo_root.to_path_buf());
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

pub fn parse_diff(repo_root: &Path, diff_range: Option<&str>) -> Result<Vec<DiffHunk>> {
    let mut args: Vec<&str> = vec!["diff"];
    args.extend_from_slice(SAFE_DIFF_FLAGS);
    args.push("--unified=0");
    args.push("-M");
    if let Some(range) = diff_range {
        validate_diff_range(range)?;
        args.push(range);
    }

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

pub fn get_changed_files(repo_root: &Path, diff_range: Option<&str>) -> Result<Vec<PathBuf>> {
    let mut args: Vec<&str> = vec!["diff"];
    args.extend_from_slice(SAFE_DIFF_FLAGS);
    args.extend_from_slice(&["--name-only", "-M", "-z"]);
    if let Some(range) = diff_range {
        validate_diff_range(range)?;
        args.push(range);
    }
    let parts = run_git_z(repo_root, &args)?;
    Ok(parts
        .iter()
        .map(|p| {
            repo_root
                .join(p)
                .canonicalize()
                .unwrap_or_else(|_| repo_root.join(p))
        })
        .collect())
}

pub fn get_deleted_files(repo_root: &Path, diff_range: Option<&str>) -> Result<FxHashSet<PathBuf>> {
    let mut args: Vec<&str> = vec!["diff"];
    args.extend_from_slice(SAFE_DIFF_FLAGS);
    args.extend_from_slice(&["--diff-filter=D", "--name-only", "-M", "-z"]);
    if let Some(range) = diff_range {
        validate_diff_range(range)?;
        args.push(range);
    }
    let parts = run_git_z(repo_root, &args)?;
    Ok(parts
        .iter()
        .map(|p| {
            repo_root
                .join(p)
                .canonicalize()
                .unwrap_or_else(|_| repo_root.join(p))
        })
        .collect())
}

/// The `R` records of a rename-only diff, as raw `(old, new)` strings.
///
/// `--name-status -z` emits renames as `R<similarity>\0old\0new\0`, so the walk
/// steps three fields per record and one otherwise. Both callers below ran their
/// own copy of that walk; they disagreed on validation, one accepting a record
/// whose destination was missing. A rename without a destination is not a
/// rename, so the stricter reading is the one kept here.
fn rename_records(repo_root: &Path, diff_range: Option<&str>) -> Result<Vec<(String, String)>> {
    let mut args: Vec<&str> = vec!["diff"];
    args.extend_from_slice(SAFE_DIFF_FLAGS);
    args.extend_from_slice(&["--diff-filter=R", "--name-status", "-M", "-z"]);
    if let Some(range) = diff_range {
        validate_diff_range(range)?;
        args.push(range);
    }
    let output = run_git(repo_root, &args)?;
    let parts: Vec<&str> = output.split('\0').collect();

    let mut records = Vec::new();
    let mut i = 0;
    while i < parts.len() {
        if parts[i].starts_with('R') {
            if i + 2 < parts.len() && !parts[i + 1].is_empty() && !parts[i + 2].is_empty() {
                records.push((parts[i + 1].to_string(), parts[i + 2].to_string()));
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    Ok(records)
}

/// Rename *source* paths, canonicalized. These no longer exist on disk and
/// cannot be fragmented, so the pipeline excludes them from the changed set.
/// The rename destinations need no special handling: they exist on HEAD and
/// reach the universe through the ordinary changed-file path.
pub fn get_renamed_paths(repo_root: &Path, diff_range: Option<&str>) -> Result<FxHashSet<PathBuf>> {
    Ok(rename_records(repo_root, diff_range)?
        .into_iter()
        .map(|(old, _)| {
            repo_root
                .join(&old)
                .canonicalize()
                .unwrap_or_else(|_| repo_root.join(&old))
        })
        .collect())
}

/// Rename pairs as repo-relative display paths (`old -> new`), for the output
/// header. Unlike `get_renamed_paths` this preserves the pairing and does not
/// canonicalize (the old path no longer exists on disk).
pub fn get_rename_pairs(
    repo_root: &Path,
    diff_range: Option<&str>,
) -> Result<Vec<(String, String)>> {
    Ok(rename_records(repo_root, diff_range)?
        .into_iter()
        .map(|(old, new)| {
            (
                crate::paths::to_posix_display(std::borrow::Cow::Owned(old)),
                crate::paths::to_posix_display(std::borrow::Cow::Owned(new)),
            )
        })
        .collect())
}

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
                .filter(|s| !s.is_empty());
            (base, head)
        }
    }
}

pub fn show_file_at_revision(repo_root: &Path, rev: &str, rel_path: &Path) -> Result<String> {
    validate_rev(rev)?;
    let spec = format!("{}:{}", rev, rel_path.to_string_lossy().replace('\\', "/"));
    run_git(repo_root, &["show", &spec])
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

pub fn get_untracked_files(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let parts = run_git_z(
        repo_root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    Ok(parts
        .iter()
        .map(|p| {
            repo_root
                .join(p)
                .canonicalize()
                .unwrap_or_else(|_| repo_root.join(p))
        })
        .collect())
}

/// Rewrites one `.diffctx/ignore` pattern line to be anchored to the
/// directory that contains the `.diffctx/` folder (`rel`, repo-root-relative,
/// "" for the repo root itself). Mirrors `_process_ignore_line` in the
/// Python tree-mode ignore resolver (`src/diffctx/ignore.py`) so a pattern
/// declared in `sub/.diffctx/ignore` only ever matches within `sub/`.
fn anchor_diffctx_ignore_line(line: &str, rel: &str) -> String {
    let (neg, pat) = match line.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, line),
    };
    let pat_no_trailing_slash = pat.trim_end_matches('/');
    let full = if pat_no_trailing_slash.starts_with('/') || pat_no_trailing_slash.contains('/') {
        let anchored = pat.trim_start_matches('/');
        if rel.is_empty() {
            format!("/{anchored}")
        } else {
            format!("/{rel}/{anchored}")
        }
    } else if rel.is_empty() {
        pat.to_string()
    } else {
        format!("{rel}/**/{pat}")
    };
    if neg { format!("!{full}") } else { full }
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

/// Finds every `.diffctx/ignore` file tracked or present in `repo_root`
/// (any depth) and returns its patterns rewritten to be repo-root-relative,
/// ready to feed into a combined gitignore-syntax exclude file.
fn collect_diffctx_ignore_patterns(repo_root: &Path) -> Vec<String> {
    let Ok(files) = run_git_z(
        repo_root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            ":(glob)**/.diffctx/ignore",
        ],
    ) else {
        return Vec::new();
    };

    let mut patterns = Vec::new();
    for raw in &files {
        let rel_path = unquote_c_style(raw);
        if !rel_path.ends_with(".diffctx/ignore") {
            continue;
        }
        let rel_dir = rel_path
            .strip_suffix(".diffctx/ignore")
            .unwrap_or("")
            .trim_end_matches('/');
        let Ok(content) = std::fs::read_to_string(repo_root.join(&rel_path)) else {
            continue;
        };
        for line in content.lines() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            patterns.push(anchor_diffctx_ignore_line(line, rel_dir));
        }
    }
    patterns
}

/// Which rule family excluded a path. `.diffctx/ignore` is a declared
/// confidentiality policy, so its exclusions are surfaced only as a count;
/// gitignore exclusions are mundane and can be listed by path (#188).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IgnoreSource {
    DiffctxPolicy,
    Gitignore,
}

/// Returns the subset of `rel_paths` (repo-root-relative) excluded by either
/// `.gitignore` (via git's own engine, so nesting/negation/`**` are handled
/// correctly) or `.diffctx/ignore` (patterns anchored per-directory and fed
/// to git as a temporary `core.excludesFile`, so the same engine evaluates
/// both mechanisms uniformly), mapped to the rule family that excluded each.
/// Best-effort: any failure returns an empty map rather than blocking the
/// diff pipeline on an ignore-resolution problem.
///
/// A `.gitignore` exclusion inherited from an excluded ancestor directory does
/// NOT count. `--no-index` is required for `.diffctx/ignore` to apply to
/// tracked files at all, but it also revives git's rule that a file cannot be
/// re-included once a parent directory is excluded. pandoc excludes every
/// dotted root entry with `/*.*` and re-includes `!.github/**`: git keeps
/// `.github/workflows/ci.yml` because it is tracked, while `--no-index`
/// reports it ignored *via the ancestor* — which silently reduced a real
/// change to an empty selection (#153). A pattern matching the path itself
/// still excludes it, so `.diffctx/ignore` and per-directory `.gitignore`
/// rules (#85) keep working.
pub fn find_ignored_paths_with_source(
    repo_root: &Path,
    rel_paths: &[String],
) -> rustc_hash::FxHashMap<String, IgnoreSource> {
    if rel_paths.is_empty() {
        return rustc_hash::FxHashMap::default();
    }

    let diffctx_patterns = collect_diffctx_ignore_patterns(repo_root);
    let temp_excludes = if diffctx_patterns.is_empty() {
        None
    } else {
        write_private_temp_file(&diffctx_patterns.join("\n"))
    };

    // Ancestors are queried alongside the paths themselves so an exclusion can
    // be attributed: same winning rule on a parent directory means the file was
    // only caught transitively.
    let mut queries: Vec<String> = rel_paths.to_vec();
    let mut ancestors: FxHashSet<String> = FxHashSet::default();
    for rel in rel_paths {
        for ancestor in ancestor_dirs(rel) {
            if ancestors.insert(ancestor.clone()) {
                queries.push(ancestor);
            }
        }
    }

    // Paths go over stdin, not argv. Every diff path plus every ancestor
    // directory used to be passed as arguments, so a monorepo-sized range hit
    // the platform argv limit, `git` failed to spawn, and the fail-open tail
    // below turned that into "nothing is ignored" — silently disabling the
    // `.diffctx/ignore` contract exactly when the repo is large enough for it
    // to matter. stdin has no such ceiling.
    // `-z` on BOTH sides. Line-delimited input splits a path containing a
    // newline into two phantom queries: git then answers about `secret` and
    // `name.py` while the real path `secret\nname.py` never gets a verdict, so
    // the lookup below misses it and a file the user declared ignored is
    // published. Verified against git directly — line mode reports the
    // truncated stem, NUL mode reports the whole path. The rest of this module
    // is already `-z` throughout.
    let mut args: Vec<String> = vec![
        "check-ignore".into(),
        "--no-index".into(),
        "-v".into(),
        "-z".into(),
        "--stdin".into(),
    ];
    if let Some(ref path) = temp_excludes {
        args.insert(0, format!("core.excludesFile={}", path.display()));
        args.insert(0, "-c".into());
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    // Paths reach `--stdin` through a file rather than a pipe we write
    // ourselves. Writing the whole payload into a pipe before reading stdout
    // deadlocks as soon as it outgrows the pipe buffer: git blocks emitting
    // matches that nobody is draining, we block on the write, and the pipeline
    // hangs until the git timeout fires. A file hands the feeding to the kernel,
    // which needs no second thread to stay correct.
    let query_file = write_private_temp_file(&format!("{}\0", queries.join("\0")));

    let result = (|| -> Result<rustc_hash::FxHashMap<String, IgnoreSource>> {
        let Some(ref query_path) = query_file else {
            return Err(GitError::CommandFailed(
                "could not stage check-ignore query paths".into(),
            ));
        };
        let mut cmd = git_command(repo_root);
        cmd.args(&arg_refs)
            .stdin(Stdio::from(std::fs::File::open(query_path)?))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = cmd.spawn()?;
        let output = wait_with_timeout(child, Duration::from_secs(git_timeout()), &arg_refs)?;
        // Exit code 1 from `check-ignore` means "none of the given paths are
        // ignored" — not a failure. Any other non-zero code is a real error.
        if !output.status.success() && output.status.code() != Some(1) {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(GitError::CommandFailed(format!(
                "git check-ignore failed: {}",
                stderr.trim()
            )));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let excludes_source = temp_excludes.as_ref().map(|p| p.display().to_string());

        let rules = parse_verbose_ignore_records(&stdout);

        Ok(rel_paths
            .iter()
            .filter_map(|rel| {
                let rule = rules.get(rel)?;
                let from_diffctx = excludes_source
                    .as_deref()
                    .is_some_and(|src| rule.starts_with(&format!("{src}:")));
                if from_diffctx {
                    Some((rel.clone(), IgnoreSource::DiffctxPolicy))
                } else if !ancestor_dirs(rel)
                    .iter()
                    .any(|dir| rules.get(dir) == Some(rule))
                {
                    Some((rel.clone(), IgnoreSource::Gitignore))
                } else {
                    None
                }
            })
            .collect())
    })();

    if let Some(path) = temp_excludes {
        let _ = std::fs::remove_file(path);
    }
    if let Some(path) = query_file {
        let _ = std::fs::remove_file(path);
    }

    // Fail closed only where a policy was actually declared.
    //
    // The returned set is "exclude these", so `unwrap_or_default()` answered a
    // failed check with "nothing is ignored" — the one answer that leaks, and
    // QA.md calls `.diffctx/ignore` a security contract. But failing closed
    // unconditionally is too blunt: `check-ignore` cannot run in a bare clone at
    // all, and excluding everything turned a supported repo shape into empty
    // output.
    //
    // So the two cases are separated by whether the user declared anything. With
    // `.diffctx/ignore` patterns present, a failed check must not silently
    // publish what they asked to withhold. Without them the only loss is
    // best-effort gitignore filtering, and refusing to emit anything would be a
    // worse answer than emitting a bare clone's diff. A bare repo has no working
    // tree, so it never has patterns and always lands on the open branch.
    let policy_declared = !diffctx_patterns.is_empty();
    result.unwrap_or_else(|e| {
        if policy_declared {
            tracing::error!(
                "git check-ignore failed ({e}); .diffctx/ignore declares {} pattern(s), so all \
                 {} queried paths are treated as ignored rather than risk publishing them",
                diffctx_patterns.len(),
                rel_paths.len()
            );
            rel_paths
                .iter()
                .map(|p| (p.clone(), IgnoreSource::DiffctxPolicy))
                .collect()
        } else {
            tracing::warn!(
                "git check-ignore failed ({e}); no .diffctx/ignore patterns are declared, so \
                 gitignore filtering is skipped for this run"
            );
            rustc_hash::FxHashMap::default()
        }
    })
}

/// `check-ignore -v` emits `<source>:<line>:<pattern>\t<path>`; returns the
/// `<source>:<line>:<pattern>` rule identity and the path it matched.
///
/// Split from the right: git prints the pattern raw but C-quotes any path
/// containing a tab, so the last tab is always the separator. Splitting from
/// the left mis-parses a pattern that itself contains a tab, and the resulting
/// lookup miss reports an ignored file as not ignored — i.e. it leaks.
/// `check-ignore -v -z` emits four NUL-separated fields per match —
/// `<source>\0<line>\0<pattern>\0<path>\0` — and never quotes the path,
/// because NUL cannot appear in one.
///
/// The text format this replaced (`<source>:<line>:<pattern>\t<path>`) had to
/// guess where the pattern ended and the path began, and C-quoting made a path
/// containing a tab or a newline ambiguous. Field-delimited records remove the
/// guess entirely.
fn parse_verbose_ignore_records(stdout: &str) -> rustc_hash::FxHashMap<String, String> {
    let mut rules: rustc_hash::FxHashMap<String, String> = rustc_hash::FxHashMap::default();
    let fields: Vec<&str> = stdout.split('\0').collect();
    // A trailing NUL leaves an empty final element; chunks of four skip it.
    for record in fields.chunks(4) {
        if record.len() < 4 {
            break;
        }
        let (source, line, pattern, path) = (record[0], record[1], record[2], record[3]);
        if path.is_empty() {
            continue;
        }
        // `check-ignore -v` also prints a record when the LAST matching
        // pattern is a negation — the path is then explicitly NOT ignored
        // (plain `check-ignore` exits 1 for it). Treating any record as "this
        // path is ignored" inverted the meaning: a repository that un-ignores
        // a file (`!SECURITY.md`) had that file silently dropped from --diff
        // output (#193). The same reading is right for `.diffctx/ignore`: a
        // negation there is the user explicitly re-including a path in the
        // policy's own terms.
        if pattern.starts_with('!') {
            continue;
        }
        // The rule identity keeps the text format's shape: callers compare it
        // against `format!("{excludes_source}:")` to tell a diffctx-declared
        // exclusion from a gitignore one.
        rules.insert(path.to_string(), format!("{source}:{line}:{pattern}"));
    }
    rules
}

fn ancestor_dirs(rel: &str) -> Vec<String> {
    let mut dirs = Vec::new();
    let mut remainder = rel;
    while let Some((parent, _)) = remainder.rsplit_once('/') {
        dirs.push(parent.to_string());
        remainder = parent;
    }
    dirs
}

pub struct CatFileBatch {
    repo_root: PathBuf,
    child: Option<Child>,
    reader: Option<BufReader<ChildStdout>>,
}

impl CatFileBatch {
    pub fn new(repo_root: &Path) -> Result<Self> {
        let mut batch = Self {
            repo_root: repo_root.to_path_buf(),
            child: None,
            reader: None,
        };
        batch.ensure_started()?;
        Ok(batch)
    }

    fn ensure_started(&mut self) -> Result<()> {
        let needs_restart = match &mut self.child {
            None => true,
            Some(child) => child.try_wait().ok().flatten().is_some(),
        };

        if needs_restart {
            let mut child = git_command(&self.repo_root)
                .args(["cat-file", "--batch"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?;
            let stdout = child.stdout.take().ok_or_else(|| {
                GitError::CommandFailed("cat-file: failed to capture stdout pipe".into())
            })?;
            self.reader = Some(BufReader::new(stdout));
            self.child = Some(child);
        }

        Ok(())
    }

    pub fn get(&mut self, rev: &str, rel_path: &Path) -> Result<String> {
        validate_rev(rev)?;
        let spec = format!(
            "{}:{}\n",
            rev,
            rel_path.to_string_lossy().replace('\\', "/")
        );

        self.ensure_started()?;

        let stdin = self
            .child
            .as_mut()
            .and_then(|c| c.stdin.as_mut())
            .ok_or_else(|| GitError::CommandFailed("cat-file stdin unavailable".into()))?;
        stdin.write_all(spec.as_bytes())?;
        stdin.flush()?;

        let reader = self
            .reader
            .as_mut()
            .ok_or_else(|| GitError::CommandFailed("cat-file stdout unavailable".into()))?;

        let mut header_line = String::new();
        reader.read_line(&mut header_line)?;

        if header_line.is_empty() {
            return Err(GitError::CommandFailed(format!(
                "cat-file: unexpected EOF for {}",
                spec.trim()
            )));
        }

        let header_str = header_line.trim();
        if header_str.ends_with("missing") {
            return Err(GitError::CommandFailed(format!(
                "Path not found: {}",
                spec.trim()
            )));
        }

        let parts: Vec<&str> = header_str.split_whitespace().collect();
        if parts.len() < 3 {
            return Err(GitError::CommandFailed(format!(
                "cat-file: malformed header: {}",
                header_str
            )));
        }

        let size: usize = parts[2].parse().map_err(|_| {
            GitError::CommandFailed(format!("cat-file: invalid size in header: {}", header_str))
        })?;

        // Guard against allocating an unbounded blob. Anything larger than the
        // biggest size we will ever parse is drained from the stream in bounded
        // chunks (to keep the cat-file pipe in sync for the next request) and
        // rejected, instead of allocating `size` bytes up front (OOM on a
        // pathological multi-hundred-MB blob).
        if size > crate::config::limits::MAX_BLOB_READ_BYTES {
            let mut remaining = size;
            let mut scratch = [0u8; 65536];
            while remaining > 0 {
                let want = remaining.min(scratch.len());
                reader.read_exact(&mut scratch[..want])?;
                remaining -= want;
            }
            let mut trailing = [0u8; 1];
            let _ = reader.read_exact(&mut trailing);
            return Err(GitError::CommandFailed(format!(
                "cat-file: blob too large ({} bytes): {}",
                size,
                spec.trim()
            )));
        }

        let mut content = vec![0u8; size];
        reader.read_exact(&mut content)?;

        let mut trailing = [0u8; 1];
        let _ = reader.read_exact(&mut trailing);

        Ok(String::from_utf8_lossy(&content).into_owned())
    }

    pub fn close(&mut self) {
        self.reader.take();
        if let Some(mut child) = self.child.take() {
            drop(child.stdin.take());
            match child.wait_timeout(Duration::from_secs(GIT.catfile_termination_timeout_seconds)) {
                Ok(Some(_)) => {}
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
    }
}

impl Drop for CatFileBatch {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Barrier;
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

        for root in [&clean_root, &hostile_root] {
            init_git_repo(root);
            write_file(root, "app.py", "def f():\n    return 1\n");
            commit_all(root, "initial");
            write_file(root, "app.py", "def f():\n    return 2\n");
            commit_all(root, "change");
        }
        for args in hostile_config {
            git(&hostile_root, args);
        }

        let clean_hunks = parse_diff(&clean_root, Some("HEAD~1..HEAD")).expect("clean parse_diff");
        let hostile_hunks =
            parse_diff(&hostile_root, Some("HEAD~1..HEAD")).expect("hostile parse_diff");
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

        let clean_files =
            get_changed_files(&clean_root, Some("HEAD~1..HEAD")).expect("clean changed files");
        let hostile_files =
            get_changed_files(&hostile_root, Some("HEAD~1..HEAD")).expect("hostile changed files");
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

        let resolved = resolve_duration_range(root, Some("24h")).expect("resolve");
        assert!(resolved.from_duration);
        assert_eq!(resolved.range.as_deref(), Some(old_head.as_str()));

        let diff = get_diff_text(root, resolved.range.as_deref()).expect("diff");
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

        let resolved = resolve_duration_range(root, Some("1w")).expect("resolve");
        assert!(resolved.from_duration);
        let diff = get_diff_text(root, resolved.range.as_deref()).expect("diff");
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

        let resolved = resolve_duration_range(root, Some("24h")).expect("resolve");
        assert!(!resolved.from_duration);
        assert_eq!(resolved.range.as_deref(), Some("24h"));
    }

    // --- parse_verbose_ignore_records: NUL fields remove every delimiter guess ---

    #[test]
    fn a_tab_in_the_pattern_no_longer_needs_disambiguating() {
        // The text format put the pattern and the path on one line separated by
        // a tab, so a pattern containing a tab had to be split from the right
        // and hoped for. Fields make it unambiguous.
        let stdout = ".gitignore\x003\x00foo\tbar\x00some/real/path.txt\x00";
        let rules = parse_verbose_ignore_records(stdout);
        assert_eq!(
            rules.get("some/real/path.txt").map(String::as_str),
            Some(".gitignore:3:foo\tbar")
        );
    }

    /// The leak that forced `-z`. A newline in a filename split the query into
    /// two phantom paths, git answered about the stem, the real path never got
    /// a verdict, and a file the user declared ignored was published.
    #[test]
    fn a_newline_in_the_path_survives_as_one_record() {
        let stdout = "excl\x001\x00secret*\x00secret\nname.py\x00";
        let rules = parse_verbose_ignore_records(stdout);
        assert_eq!(rules.len(), 1);
        assert_eq!(
            rules.get("secret\nname.py").map(String::as_str),
            Some("excl:1:secret*")
        );
    }

    #[test]
    fn several_records_and_a_trailing_nul_parse_cleanly() {
        let stdout = ".gitignore\x001\x00*.log\x00a.log\x00.gitignore\x002\x00*.tmp\x00b/c.tmp\x00";
        let rules = parse_verbose_ignore_records(stdout);
        assert_eq!(rules.len(), 2);
        assert!(rules.contains_key("a.log"));
        assert!(rules.contains_key("b/c.tmp"));
    }

    #[test]
    fn a_truncated_final_record_is_dropped_not_half_read() {
        // Killed mid-write, the last record is short. Reading three fields as
        // four would key a rule under a pattern.
        let stdout = ".gitignore\x001\x00*.log\x00a.log\x00.gitignore\x002\x00*.tmp\x00";
        let rules = parse_verbose_ignore_records(stdout);
        assert_eq!(rules.len(), 1);
        assert!(rules.contains_key("a.log"));
    }

    // --- anchor_diffctx_ignore_line: 4 reachable outputs, root vs nested, negation ---

    #[test]
    fn anchor_ignore_line_bare_pattern_at_root() {
        assert_eq!(anchor_diffctx_ignore_line("*.log", ""), "*.log");
    }

    #[test]
    fn anchor_ignore_line_bare_pattern_nested() {
        assert_eq!(anchor_diffctx_ignore_line("*.log", "sub"), "sub/**/*.log");
    }

    #[test]
    fn anchor_ignore_line_slash_pattern_at_root() {
        assert_eq!(
            anchor_diffctx_ignore_line("secrets/config.py", ""),
            "/secrets/config.py"
        );
    }

    #[test]
    fn anchor_ignore_line_slash_pattern_nested() {
        assert_eq!(
            anchor_diffctx_ignore_line("secrets/config.py", "sub"),
            "/sub/secrets/config.py"
        );
    }

    #[test]
    fn anchor_ignore_line_negated_bare_pattern() {
        assert_eq!(anchor_diffctx_ignore_line("!keep.log", ""), "!keep.log");
    }

    #[test]
    fn anchor_ignore_line_negated_slash_pattern_nested() {
        assert_eq!(
            anchor_diffctx_ignore_line("!secrets/keep.py", "sub"),
            "!/sub/secrets/keep.py"
        );
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
        let base = tmp.path().canonicalize().expect("canonical tempdir");
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
        let base = tmp.path().canonicalize().expect("canonical tempdir");
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

        let hunks = parse_diff(root, Some("HEAD~1..HEAD")).expect("parse_diff");
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
    fn wait_with_timeout_does_not_penalize_fast_commands() {
        let child = Command::new("true")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn true");
        let result = wait_with_timeout(child, Duration::from_secs(5), &["true"]);
        assert!(matches!(result, Ok(ref out) if out.status.success()));
    }

    // --- PID-keyed temp excludesFile: two concurrent calls in one process ---

    #[test]
    fn ignored_paths_concurrent_calls_both_see_their_own_ignore_rules() {
        let tmp = TempDir::new().expect("tempdir");
        let root_a = tmp.path().join("repo_a");
        let root_b = tmp.path().join("repo_b");
        fs::create_dir_all(&root_a).expect("mkdir a");
        fs::create_dir_all(&root_b).expect("mkdir b");

        for (root, secret) in [(&root_a, "secret_a.py"), (&root_b, "secret_b.py")] {
            init_git_repo(root);
            write_file(root, "app.py", "print('hi')\n");
            write_file(root, ".diffctx/ignore", &format!("{secret}\n"));
            write_file(root, secret, "SECRET\n");
            commit_all(root, "initial");
        }

        // Run several concurrent rounds: a single lucky interleaving proved
        // the pre-fix PID-only path could collide, so repeat to make a
        // regression reliably visible instead of a one-shot coin flip.
        for _ in 0..10 {
            let barrier = Arc::new(Barrier::new(2));

            let root_a_thread = root_a.clone();
            let barrier_a = Arc::clone(&barrier);
            let handle_a = std::thread::spawn(move || {
                barrier_a.wait();
                find_ignored_paths_with_source(
                    &root_a_thread,
                    &["secret_a.py".to_string(), "app.py".to_string()],
                )
            });

            let root_b_thread = root_b.clone();
            let barrier_b = Arc::clone(&barrier);
            let handle_b = std::thread::spawn(move || {
                barrier_b.wait();
                find_ignored_paths_with_source(
                    &root_b_thread,
                    &["secret_b.py".to_string(), "app.py".to_string()],
                )
            });

            let ignored_a = handle_a.join().expect("thread a panicked");
            let ignored_b = handle_b.join().expect("thread b panicked");

            assert_eq!(
                ignored_a.get("secret_a.py"),
                Some(&IgnoreSource::DiffctxPolicy),
                "repo A lost its .diffctx/ignore rule to a concurrent call"
            );
            assert_eq!(
                ignored_b.get("secret_b.py"),
                Some(&IgnoreSource::DiffctxPolicy),
                "repo B lost its .diffctx/ignore rule to a concurrent call"
            );
            assert!(!ignored_a.contains_key("app.py"));
            assert!(!ignored_b.contains_key("app.py"));
        }
    }
}

#[cfg(test)]
mod negation_record_tests {
    use super::*;

    #[test]
    fn a_negation_record_does_not_mark_the_path_ignored() {
        // check-ignore -v -z output: source \0 line \0 pattern \0 path \0 ...
        let stdout = ".gitignore\x001\x00*.tmp\x00drop.tmp\x00.gitignore\x002\x00!NEWDOC.md\x00NEWDOC.md\x00";
        let rules = parse_verbose_ignore_records(stdout);
        assert!(
            rules.contains_key("drop.tmp"),
            "a positive match must stay an exclusion"
        );
        assert!(
            !rules.contains_key("NEWDOC.md"),
            "a negation match means the path is explicitly NOT ignored (#193)"
        );
    }
}
