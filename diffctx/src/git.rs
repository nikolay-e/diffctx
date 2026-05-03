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

static GIT_TIMEOUT_SECS: AtomicU64 = AtomicU64::new(git::DEFAULT_TIMEOUT_SECONDS);

pub fn set_git_timeout(secs: u64) {
    GIT_TIMEOUT_SECS.store(secs, Ordering::Relaxed);
}

fn git_timeout() -> u64 {
    GIT_TIMEOUT_SECS.load(Ordering::Relaxed)
}
const SAFE_DIFF_FLAGS: &[&str] = &["--no-textconv", "--no-ext-diff"];

static HUNK_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@").unwrap());

static RANGE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s*(\S+?)(\.\.\.?)(\S*?)\s*$").unwrap());

static SAFE_RANGE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[a-zA-Z0-9_.^~/@{}\-]+(\.\.\.?[a-zA-Z0-9_.^~/@{}\-]*)?$").unwrap());

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("git command failed: {0}")]
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
    Ok(())
}

pub fn run_git(repo_root: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo_root)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

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
        return Err(GitError::CommandFailed(format!(
            "git {} failed: {}",
            args.join(" "),
            stderr.trim()
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

pub fn get_diff_text(repo_root: &Path, diff_range: Option<&str>) -> Result<String> {
    let mut args: Vec<&str> = vec!["diff"];
    args.extend_from_slice(SAFE_DIFF_FLAGS);
    if let Some(range) = diff_range {
        validate_diff_range(range)?;
        args.push(range);
    }
    run_git(repo_root, &args)
}

fn unquote_c_style(quoted: &str) -> String {
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

fn parse_path_line(line: &str, repo_root: &Path) -> (&'static str, Option<PathBuf>) {
    let resolved_root = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());

    if line.starts_with("--- /dev/null") {
        return ("old", None);
    }
    if line.starts_with("+++ /dev/null") {
        return ("new", None);
    }

    if let Some(rest) = line.strip_prefix("--- a/") {
        let rel_path = rest.trim();
        let resolved = (repo_root.join(rel_path))
            .canonicalize()
            .unwrap_or_else(|_| repo_root.join(rel_path));
        if !resolved.starts_with(&resolved_root) {
            return ("", None);
        }
        return ("old", Some(repo_root.join(rel_path)));
    }

    if let Some(rest) = line.strip_prefix("+++ b/") {
        let rel_path = rest.trim();
        let resolved = (repo_root.join(rel_path))
            .canonicalize()
            .unwrap_or_else(|_| repo_root.join(rel_path));
        if !resolved.starts_with(&resolved_root) {
            return ("", None);
        }
        return ("new", Some(repo_root.join(rel_path)));
    }

    if line.starts_with("--- \"a/") {
        let quoted = line.strip_prefix("--- ").unwrap().trim();
        let unquoted = unquote_c_style(quoted);
        let rel_path = unquoted.strip_prefix("a/").unwrap_or(&unquoted);
        let resolved = (repo_root.join(rel_path))
            .canonicalize()
            .unwrap_or_else(|_| repo_root.join(rel_path));
        if !resolved.starts_with(&resolved_root) {
            return ("", None);
        }
        return ("old", Some(repo_root.join(rel_path)));
    }

    if line.starts_with("+++ \"b/") {
        let quoted = line.strip_prefix("+++ ").unwrap().trim();
        let unquoted = unquote_c_style(quoted);
        let rel_path = unquoted.strip_prefix("b/").unwrap_or(&unquoted);
        let resolved = (repo_root.join(rel_path))
            .canonicalize()
            .unwrap_or_else(|_| repo_root.join(rel_path));
        if !resolved.starts_with(&resolved_root) {
            return ("", None);
        }
        return ("new", Some(repo_root.join(rel_path)));
    }

    ("", None)
}

fn parse_hunk_header(caps: &regex::Captures, path: &Path) -> DiffHunk {
    let old_start: u32 = caps[1].parse().unwrap();
    let old_len: u32 = caps.get(2).map_or(1, |m| m.as_str().parse().unwrap());
    let new_start: u32 = caps[3].parse().unwrap();
    let new_len: u32 = caps.get(4).map_or(1, |m| m.as_str().parse().unwrap());

    DiffHunk {
        path: Arc::from(path.to_string_lossy().as_ref()),
        new_start,
        new_len,
        old_start,
        old_len,
    }
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
    let mut hunks = Vec::new();
    let mut old_path: Option<PathBuf> = None;
    let mut new_path: Option<PathBuf> = None;

    for line in output.lines() {
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
                hunks.push(parse_hunk_header(&caps, p));
            }
        }
    }

    Ok(hunks)
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
    Ok(parts.iter().map(|p| repo_root.join(p)).collect())
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

pub fn get_renamed_paths(
    repo_root: &Path,
    diff_range: Option<&str>,
    min_similarity: u32,
) -> Result<(FxHashSet<PathBuf>, FxHashSet<PathBuf>)> {
    let mut args: Vec<&str> = vec!["diff"];
    args.extend_from_slice(SAFE_DIFF_FLAGS);
    args.extend_from_slice(&["--diff-filter=R", "--name-status", "-M", "-z"]);
    if let Some(range) = diff_range {
        validate_diff_range(range)?;
        args.push(range);
    }
    let output = run_git(repo_root, &args)?;
    let parts: Vec<&str> = output.split('\0').collect();

    let mut old_paths = FxHashSet::default();
    let mut pure_new_paths = FxHashSet::default();
    let mut i = 0;

    while i < parts.len() {
        if parts[i].starts_with('R') {
            let sim: u32 = parts[i][1..].parse().unwrap_or(0);

            if i + 1 < parts.len() && !parts[i + 1].is_empty() {
                let resolved = repo_root
                    .join(parts[i + 1])
                    .canonicalize()
                    .unwrap_or_else(|_| repo_root.join(parts[i + 1]));
                old_paths.insert(resolved);
            }

            if sim >= min_similarity && i + 2 < parts.len() && !parts[i + 2].is_empty() {
                let resolved = repo_root
                    .join(parts[i + 2])
                    .canonicalize()
                    .unwrap_or_else(|_| repo_root.join(parts[i + 2]));
                pure_new_paths.insert(resolved);
            }

            i += 3;
        } else {
            i += 1;
        }
    }

    Ok((old_paths, pure_new_paths))
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
    let spec = format!("{}:{}", rev, rel_path.to_string_lossy().replace('\\', "/"));
    run_git(repo_root, &["show", &spec])
}

pub fn get_commit_message(repo_root: &Path, rev: &str) -> Result<String> {
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
    Ok(parts.iter().map(|p| repo_root.join(p)).collect())
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
            let mut child = Command::new("git")
                .arg("-C")
                .arg(&self.repo_root)
                .args(["cat-file", "--batch"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?;
            let stdout = child.stdout.take().unwrap();
            self.reader = Some(BufReader::new(stdout));
            self.child = Some(child);
        }

        Ok(())
    }

    pub fn get(&mut self, rev: &str, rel_path: &Path) -> Result<String> {
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
