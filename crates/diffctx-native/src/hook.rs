//! `diffctx hook <event>`: the Claude Code hooks that put a change's impact
//! in front of the agent without asking it to call anything (#310).
//!
//! * `pretooluse`: before any Bash line, where the repository the call
//!   starts in, and its linked work trees, stand, and nothing else; before
//!   `gh pr create`, the impact of the commits the PR proposes.
//! * `posttooluse`: once that line ran, the impact of what git recorded —
//!   read off the reflog, never predicted from the command text (#508).
//!   After a `git diff` or `git status` the agent ran on its own, the impact
//!   of the working tree it just asked about. After a text search for a name
//!   the pending change edits, that name's callers.
//! * `posttoolusefailure`: the same, for a line whose last command exited
//!   non-zero (#532).
//!
//! Each reads the event on stdin and answers with `additionalContext`: the
//! impact, a pointer when what landed was already shown, or one line when it
//! reaches nothing or the line could not be read (#464, #501); nothing when
//! the line recorded nothing or anything at all goes wrong. A hook that
//! blocks a commit because it failed would be worse than no hook; the one
//! deliberate block, the opt-in strict gate, reviews the index a plain
//! `git commit` records before it runs and denies once with the impact
//! attached, so the retry passes. A commit line that stages for itself is
//! asked to stage first instead.
//!
//! Nothing here turns a failed read into an empty value: the lint that says so
//! is denied in this module. A failure is a named `Silence` the log records, an
//! answer that says what it could not read, or a one-line `allow` that says why
//! losing the value costs a repeated review and never a wrong answer.

#![deny(clippy::disallowed_methods)]

mod intent;

use std::io::Write as _;

use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::config::limits::{DEFAULT_PPR_ALPHA, DEFAULT_SCORING};
use crate::impact::AUTOMATIC_TOKEN_CAP;
use crate::mode::ScoringMode;
use crate::snapshot as snap;
use intent::{Intent, Search, before_glob, is_absolute_dir};

/// A reviewed content hash stays reviewed this long. Long enough for a
/// review, a fix and the push that follows; short enough that the same
/// change revisited tomorrow gets its impact again.
const SEEN_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// How long a manual `--mode impact` run silences the hook on the same
/// content. It is not tied to a session, so it must not outlive the moment:
/// a run just before the commit counts, another session hours later does
/// not inherit it (#323).
const MANUAL_SEEN_TTL: Duration = Duration::from_secs(15 * 60);

/// The hook's own wall-clock ceiling. Claude Code kills a hook at its
/// configured timeout and treats the kill as "no output", so this only has to
/// be shorter than that.
pub const HOOK_DEADLINE_SECS: u64 = 30;

/// What the pipeline itself may spend of a whole `HOOK_DEADLINE_SECS`: the
/// engine's deadline is cooperative and it renders what it read after it, so
/// the gap is what keeps the answer under the ceiling. A review gets this
/// share of what is left of the ceiling, not of the whole (Forgejo #113).
pub const PIPELINE_SECS: u64 = 20;

/// How long past `HOOK_DEADLINE_SECS` a review that stopped at its own
/// deadline may take to hand back what it read. Behind it stands Claude
/// Code's own kill at the hook's configured timeout (60 s in the plugin),
/// which prints nothing.
const TAIL_GRACE_SECS: u64 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    PreToolUse,
    /// `failed`: the line exited non-zero, which Claude Code reports as
    /// `PostToolUseFailure`. What it recorded is in git's record all the
    /// same, so it is answered like any other line that ran (#532).
    PostToolUse {
        failed: bool,
    },
}

impl Event {
    fn name(self) -> &'static str {
        match self {
            Event::PreToolUse => "PreToolUse",
            Event::PostToolUse { failed: false } => "PostToolUse",
            Event::PostToolUse { failed: true } => "PostToolUseFailure",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Trigger {
    /// `git commit` under the strict gate, the one event reviewed before git
    /// runs: the index it will record. `plain` is a line that stages nothing
    /// itself and names no paths and no `-a`; anything else is asked to
    /// stage first, so the gate never predicts what a command will stage.
    Commit { amend: bool, plain: bool },
    /// What a commit, merge, cherry-pick, revert or push recorded, read off
    /// git's own reflog once the line ran. Predicting it from the command
    /// text took fifteen parser commits in nine days and still reviewed another
    /// session's staged file for `--pathspec-from-file` (#508). `also`
    /// counts the other refs one push moved, which are not reviewed; `late`
    /// is a background line's, answered at a later event (#617).
    Landed {
        verb: &'static str,
        also: usize,
        late: bool,
        /// Commits a push published that this session's calls did not make.
        unmade: Vec<String>,
    },
    /// `gh pr create`: `head` (the current branch unless `--head` names
    /// one) against `base` (the remote's default unless `--base` names one).
    PullRequest {
        base: Option<String>,
        head: Option<String>,
    },
    /// `git diff` / `git status` the agent ran itself: the working tree
    /// (`--cached`/`--staged` narrows it to the index). `own` is what this
    /// session's transcript shows it edited, read once the repository is
    /// known; `None` when no transcript could tell (#404).
    Inspect { staged: bool, own: Option<Owned> },
}

/// The changed files an inspecting session wrote itself, in a checkout
/// other sessions may share.
#[derive(Debug, PartialEq, Eq)]
pub struct Owned {
    pub files: Vec<String>,
    /// Changed files no write of this session touched: read as committed.
    pub others: Vec<String>,
    /// Of `files`, those only a call that found them already modified is
    /// seen to have changed: what differs from `HEAD` there is more than that
    /// call did.
    pub prior: Vec<String>,
    /// Bash calls of the session the hook could not bracket: what one of
    /// them wrote cannot be told from another session's edit.
    pub unbracketed: usize,
}

/// Keywords a definition search spells around the name: `def total`,
/// `fn total`, `class Cart`.
const DECLARATION_WORDS: &str = "def class fn func function struct enum trait impl interface type let const var pub async static export import from new self this return";

/// The identifiers a search pattern is a search for: one per alternation
/// branch that is that name and nothing else once its regex escapes (`\b`,
/// `\(`, `\w`) and the declaration keywords before it (`def`, `pub fn`) are
/// dropped. A word inside a line of code is no lookup of it (`^from models
/// import`, #646). A qualified chain searches for its last segment —
/// `tests.direct` and `a::direct` for `direct`, not `tests` (#402) — only
/// when code alone would spell it: `AGENTS.md` names a file and
/// `limiter.limit` a call, not the symbols `md` and `limit` (#643). A test
/// module or test function is no definition a caller list answers for.
fn pattern_names(pattern: &str) -> Vec<String> {
    let is_keyword = |w: &str| DECLARATION_WORDS.split(' ').any(|d| d == w);
    let mut names: Vec<String> = Vec::new();
    for branch in pattern.split("\\|").flat_map(|b| b.split('|')) {
        let joined = branch.replace("\\.", ".").replace("::", ".");
        let mut cleaned = String::with_capacity(joined.len());
        let mut chars = joined.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                chars.next();
                cleaned.push(' ');
            } else if c.is_alphanumeric() || c == '_' || c == '.' {
                cleaned.push(c);
            } else {
                cleaned.push(' ');
            }
        }
        let mut words = cleaned.split_whitespace().skip_while(|w| is_keyword(w));
        let (Some(chain), None) = (words.next(), words.next()) else {
            continue;
        };
        let segments: Vec<&str> = chain
            .split('.')
            .filter(|s| !s.is_empty())
            .skip_while(|s| is_keyword(s))
            .collect();
        let Some(&w) = segments.last() else {
            continue;
        };
        if (segments.len() > 1 && !crate::impact::distinctive(w))
            || w.chars().count() < 2
            || w.starts_with(|c: char| c.is_ascii_digit())
            || is_keyword(w)
            || matches!(w, "test" | "tests")
            || w.starts_with("test_")
            || names.iter().any(|n| n == w)
        {
            continue;
        }
        names.push(w.to_string());
    }
    names.truncate(8);
    names
}

/// A pattern that is one identifier and nothing else, with or without word
/// anchors (`\b`): no alternation, no regex metacharacter.
fn is_lookup_pattern(pattern: &str) -> bool {
    let bare = pattern
        .trim()
        .trim_start_matches("\\b")
        .trim_end_matches("\\b");
    !bare.is_empty() && bare.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// A read git answers "no" to by exit status 1 (`rev-parse --verify --quiet`
/// of an absent ref, `merge-base` of unrelated histories) is `None`: an
/// absence (#416). Any other failure is one the review cannot go on past.
fn git_checked(root: &Path, args: &[&str]) -> Result<Option<String>, Silence> {
    match snap::run_git_or_none(root, args, 1) {
        Ok(out) => Ok(out.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())),
        Err(_) => Err("error:git"),
    }
}

fn first_reachable(root: &Path, revs: &[&str]) -> Result<Option<String>, Silence> {
    for rev in revs {
        if git_checked(root, &["rev-parse", "--verify", "--quiet", rev])?.is_some() {
            return Ok(Some(rev.to_string()));
        }
    }
    Ok(None)
}

/// The index captured as a tree against `base`, or against the snapshot's
/// own base (`HEAD`, the empty tree on an unborn branch). Captured from a
/// copy: `write-tree` on the real index takes its lock from under a
/// concurrent `git add`.
fn staged_range(root: &Path, base: Option<&str>) -> Result<String, Silence> {
    let staged = snap::capture_planned(root, &[]).map_err(|_| "error:git")?;
    Ok(format!("{}..{}", base.unwrap_or(&staged.base), staged.tree))
}

/// This session's files as they stand (as staged, for `--cached`) and every
/// other path as `HEAD` has it: the pending change without the edits other
/// sessions left in the same checkout, captured on a copy of the index.
fn own_range(root: &Path, staged: bool, files: &[String]) -> Result<String, Silence> {
    let step = |args: &[&str], magic: &str| -> snap::PlanStep {
        let paths = files.iter().map(|f| format!(":({magic}){f}"));
        let args = args.iter().map(|a| a.to_string()).chain(paths).collect();
        (root.to_path_buf(), args)
    };
    let mut plan = Vec::new();
    if !staged {
        plan.push(step(&["add", "-A", "--"], "literal"));
    }
    plan.push(step(&["reset", "-q", "--", ":/"], "exclude,literal"));
    let snapshot = snap::capture_planned(root, &plan).map_err(|_| "error:git")?;
    Ok(format!("{}..{}", snapshot.base, snapshot.tree))
}

/// A directory a search names, relative to the payload's `cwd`.
fn resolve_dir(cwd: &Path, dir: Option<&str>) -> PathBuf {
    match dir {
        Some(dir) if is_absolute_dir(dir) => PathBuf::from(dir),
        Some(dir) => cwd.join(dir),
        None => cwd.to_path_buf(),
    }
}

/// The range a trigger read before git runs is about. `cwd` is where the
/// agent ran the line; a landed record carries its own range. A ref nothing
/// here resolves is `no-range` (`unresolved-ref` for a pull request, #416:
/// never replaced by the current branch); a read git refused is `error:git`.
fn range_for(root: &Path, trigger: &Trigger) -> Result<String, Silence> {
    Ok(match trigger {
        Trigger::Inspect {
            staged,
            own: Some(own),
        } if !own.others.is_empty() => own_range(root, *staged, &own.files)?,
        Trigger::Inspect { staged: false, .. } => "HEAD".to_string(),
        Trigger::Inspect { staged: true, .. } => staged_range(root, None)?,
        Trigger::Commit { amend, .. } => {
            let base = if *amend {
                Some(
                    git_checked(root, &["rev-parse", "--verify", "--quiet", "HEAD~1"])?
                        .ok_or("no-range")?,
                )
            } else {
                None
            };
            staged_range(root, base.as_deref())?
        }
        Trigger::PullRequest { base, head } => {
            // `--head` opens the PR from the branch as pushed.
            let head = match head {
                Some(h) => {
                    first_reachable(root, &[format!("refs/remotes/origin/{h}").as_str(), h])?
                        .ok_or("unresolved-ref")?
                }
                None => first_reachable(root, &["HEAD"])?.ok_or("unresolved-ref")?,
            };
            // `merge-base` of a ref nothing resolves is a refusal, not an
            // absence: the refs are asked first.
            let base = match base {
                Some(b) => first_reachable(root, &[format!("refs/remotes/origin/{b}").as_str()])?
                    .ok_or("unresolved-ref")?,
                None => first_reachable(root, &["origin/HEAD", "origin/main", "origin/master"])?
                    .ok_or("unresolved-ref")?,
            };
            let merge_base =
                git_checked(root, &["merge-base", &base, &head])?.ok_or("unresolved-ref")?;
            format!("{merge_base}..{head}")
        }
        Trigger::Landed { .. } => return Err("no-range"),
    })
}

/// Where a repository stood when a call began: `HEAD` (empty on an unborn
/// branch) and every remote-tracking ref, with how far their reflogs had
/// run. What the call recorded is whatever git's record shows past them.
#[derive(serde::Serialize, serde::Deserialize)]
struct Before {
    root: PathBuf,
    head: String,
    /// Every remote-tracking ref, as the snapshot layer splits it.
    #[serde(default)]
    tracking: Vec<Tracked>,
    /// How far `HEAD`'s reflog had run: every entry past it happened during
    /// the call (a cursor of the snapshot layer).
    #[serde(default)]
    head_cursor: u64,
    /// Where that reflog is, so a later event can see it grow without asking git.
    #[serde(default)]
    head_log: Option<PathBuf>,
    /// The repository's git directory, which outlives a work tree the line
    /// removes after pushing from it (#642).
    #[serde(default)]
    common: Option<PathBuf>,
}

/// A remote-tracking ref when the call started or ended: a push is an
/// "update by push" entry in its reflog past `log` (T20).
#[derive(serde::Serialize, serde::Deserialize)]
struct Tracked {
    /// `refs/remotes/<remote>/<branch>`.
    name: String,
    remote: String,
    branch: String,
    oid: String,
    /// How far the ref's reflog had run.
    log: u64,
}

impl Before {
    /// The repository the record was taken in, without asking git again
    /// where it lives.
    fn repo_at(&self, work_tree: &Path) -> Result<snap::RepoRoot, Silence> {
        match &self.common {
            Some(common_dir) => Ok(snap::RepoRoot {
                work_tree: work_tree.to_path_buf(),
                common_dir: common_dir.clone(),
            }),
            None => snap::open(work_tree).map_err(|_| "error:git"),
        }
    }

    /// Where the record can still be read: its work tree, or once the line
    /// removed it, the repository's main one, which holds the same remote refs.
    fn live_root(&self) -> Option<PathBuf> {
        if self.root.is_dir() {
            return Some(self.root.clone());
        }
        let common = self.common.as_deref()?;
        (common.file_name()? == ".git")
            .then(|| common.parent().map(Path::to_path_buf))
            .flatten()
            .filter(|main| main.is_dir())
    }
}

/// The record a line's `PreToolUse` leaves for its `PostToolUse`, or for
/// a later event when the line runs in the background (#617).
#[derive(serde::Serialize, serde::Deserialize)]
struct Record {
    repos: Vec<Before>,
    #[serde(default)]
    at: u64,
}

/// How many bytes a reflog holds: how far it had run. A log that does not
/// exist holds none; one that cannot be stat'ed is a refusal, never length 0,
/// which would count every earlier move as the call's.
#[allow(
    clippy::disallowed_methods,
    reason = "git's own log file, stat'ed to see it grow without a git call"
)]
fn log_len(path: &Path) -> Result<u64, Silence> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(meta.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(_) => Err("error:git"),
    }
}

/// The git directory of a checked-out work tree: its `.git`, or the directory
/// a linked work tree's `.git` file names.
#[allow(
    clippy::disallowed_methods,
    reason = "a `.git` file that cannot be read falls through to asking git, which names its own failure"
)]
fn git_dir_of(work_tree: &Path) -> Option<PathBuf> {
    let dot_git = work_tree.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let dir = Path::new(text.strip_prefix("gitdir:")?.trim());
    Some(if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        work_tree.join(dir)
    })
}

/// The work tree's own `HEAD` reflog, the record of every move it made. Read
/// off its `.git`: one git call per work tree is what made every Bash call
/// pay for each linked work tree of a repository.
fn head_log(work_tree: &Path) -> Result<PathBuf, Silence> {
    if let Some(dir) = git_dir_of(work_tree) {
        return Ok(dir.join("logs").join("HEAD"));
    }
    git_checked(
        work_tree,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "logs/HEAD",
        ],
    )?
    .map(PathBuf::from)
    .ok_or("error:git")
}

/// Where a repository stands: its refs, and how far the reflogs that tell a
/// commit from a checkout from a push had run. Read through the snapshot
/// layer, so a failed git call is a named failure and never an empty ref list.
fn refs_of(repo: &snap::RepoRoot) -> Result<Before, Silence> {
    let refs = snap::refs(repo).map_err(|_| "error:git")?;
    let tracking = refs
        .tracking
        .iter()
        .map(|(remote, branch, oid)| -> Result<Tracked, Silence> {
            let name = format!("refs/remotes/{}/{branch}", remote.0);
            Ok(Tracked {
                log: log_len(&repo.common_dir.join("logs").join(&name))?,
                remote: remote.0.clone(),
                branch: branch.clone(),
                oid: oid.to_string(),
                name,
            })
        })
        .collect::<Result<Vec<_>, Silence>>()?;
    let log = head_log(&repo.work_tree)?;
    Ok(Before {
        root: repo.work_tree.clone(),
        // An unborn branch has no commit: its head is empty.
        head: refs.head.map_or_else(String::new, |oid| oid.to_string()),
        tracking,
        head_cursor: log_len(&log)?,
        head_log: Some(log),
        common: Some(repo.common_dir.clone()),
    })
}

/// The checked-out work trees of a repository, this one included, with the
/// commit each `HEAD` is at (empty on an unborn branch): one `git worktree
/// list` where opening each tree asked git twice per tree. A bare or pruned
/// entry has no work tree and is left out.
fn work_trees(home: &snap::RepoRoot) -> Result<Vec<(PathBuf, String)>, Silence> {
    let listing = snap::run_git_z(&home.work_tree, &["worktree", "list", "--porcelain", "-z"])
        .map_err(|_| "error:git")?;
    let mut trees: Vec<(PathBuf, String, bool)> = Vec::new();
    for line in &listing {
        if let Some(path) = line.strip_prefix("worktree ") {
            trees.push((PathBuf::from(path), String::new(), true));
        } else if let Some(tree) = trees.last_mut() {
            if let Some(head) = line.strip_prefix("HEAD ") {
                tree.1 = head.to_string();
            } else if line == "bare" || line.starts_with("prunable") {
                tree.2 = false;
            }
        }
    }
    Ok(trees
        .into_iter()
        .filter(|(_, _, usable)| *usable)
        .map(|(path, head, _)| {
            let unborn = head.bytes().all(|b| b == b'0');
            (canonical(&path), if unborn { String::new() } else { head })
        })
        .collect())
}

/// `HEAD`'s reflog entries since `before`, oldest first.
#[allow(
    clippy::disallowed_methods,
    reason = "a log that cannot be stat'ed only skips the fast path: `reflog_since` below reads it"
)]
fn head_moves(before: &Before, repo: &snap::RepoRoot) -> Result<Vec<snap::ReflogEntry>, Silence> {
    // A log that has not grown holds no move: nothing to read and no git to
    // ask where it is.
    let unmoved = before
        .head_log
        .as_deref()
        .and_then(|log| std::fs::metadata(log).ok())
        .is_some_and(|m| m.len() == before.head_cursor);
    if unmoved {
        return Ok(Vec::new());
    }
    snap::reflog_since(
        repo,
        &snap::RefName("HEAD".to_string()),
        &snap::Cursor(before.head_cursor),
    )
    .map(|(entries, _)| entries)
    .map_err(|_| "error:git")
}

/// The record one line leaves for its own `PostToolUse`: per tool call, so
/// another session's commit is never this line's; a payload without a call
/// id (an older harness) is keyed by its command.
fn before_key(payload: &serde_json::Value, command: &str, session: Option<&str>) -> String {
    let call = file_safe(payload["tool_use_id"].as_str());
    if !call.is_empty() {
        return session_key(&format!("before-call-{call}"), session);
    }
    let line = Fnv::new().feed(command.as_bytes()).0;
    session_key(&format!("before-{line:016x}"), session)
}

/// What one line landed in one repository.
struct Landing {
    root: PathBuf,
    /// The range as it is named and keyed.
    range: String,
    /// The range as the engine opens it, for a push: the two ids git's
    /// record gave, not the text above read again.
    spec: Option<snap::RangeSpec>,
    verb: &'static str,
    /// Other ranges the same push moved: counted, not reviewed.
    also: usize,
    /// Commits a push published that no call of this session was seen to
    /// make, `<abbreviated id> '<subject>'` each (gh#669).
    unmade: Vec<String>,
}

/// The verb a `HEAD` reflog action is reviewed under. `None` for the moves
/// that record no change of this call's (checkout, reset, a plain pull,
/// stash; a rebase is read from its own entries).
fn commit_move(action: &str) -> Option<&'static str> {
    Some(match action {
        "commit" | "commit (initial)" | "commit (amend)" | "commit (merge)" | "am" => "commit",
        "cherry-pick" => "cherry-pick",
        "revert" => "revert",
        a if a.starts_with("merge ") => "merge",
        _ => return None,
    })
}

/// `rebase (start)`, `(pick)`, `(finish)`, and the same under `pull --rebase`.
fn is_rebase_step(action: &str) -> bool {
    action.starts_with("rebase") || action.starts_with("pull --rebase")
}

fn ancestor_of(repo: &snap::RepoRoot, older: &str, newer: &str) -> Result<bool, Silence> {
    let (Ok(older), Ok(newer)) = (snap::Oid::parse(older), snap::Oid::parse(newer)) else {
        return Err("error:git");
    };
    snap::is_ancestor(repo, older, newer).map_err(|_| "error:git")
}

/// What happened in `before.root` while the line ran, by git's own reflog:
/// a push (the `old..new` git printed for it, else what a remote-tracking
/// ref moved over by a push since the line started), and a commit, merge,
/// cherry-pick, revert, `am` or rebase on `HEAD` that the push does not
/// already hold. A checkout, reset or a plain pull moved `HEAD` without
/// recording this session's change, and is not reviewed. The count is of the
/// other ranges the same push moved: refs moved over one range (a mirror's
/// two remotes) are one (#608). Git cannot say whose a move was when a peer
/// shares the checkout: the review says it covers what changed during the
/// call.
fn landed(
    before: &Before,
    output: &Output,
    session: Option<&str>,
    pushes: Option<&[PathBuf]>,
) -> Result<Vec<Landing>, Silence> {
    let mut found = Vec::new();
    let Some(live) = before.live_root() else {
        return Ok(found);
    };
    let root = live.as_path();
    let repo = before.repo_at(root)?;
    // A work tree the line removed leaves its `HEAD` log behind with it.
    let entries = if live == before.root {
        head_moves(before, &repo)
    } else {
        Ok(Vec::new())
    };
    let mut pushed: Vec<(String, snap::RangeSpec)> = Vec::new();
    // Remote-tracking refs are shared by every work tree of a repository:
    // the call's own work tree reads the pushes, once, and says which other
    // work trees there are.
    if let Some(others) = pushes {
        let now = refs_of(&repo)?;
        for range in pushed_ranges(&repo, before, &now, output, others)? {
            if !pushed.contains(&range) {
                pushed.push(range);
            }
        }
    }
    // The tip the first push published: a commit the line made that this
    // tip holds is part of that review.
    let mut published: Option<String> = None;
    if let Some((range, spec)) = pushed.first() {
        let rewritten = matches!(spec, snap::RangeSpec::Rewritten { .. });
        // A push that cannot tell what this call committed says nothing of
        // who made what it published.
        let unmade = match &entries {
            Ok(entries) if !rewritten => unmade_commits(root, range, entries, session)?,
            _ => Vec::new(),
        };
        published = match spec {
            snap::RangeSpec::Pushed { new, .. }
            | snap::RangeSpec::Rewritten { new, .. }
            | snap::RangeSpec::Review { head: new, .. } => Some(new.to_string()),
            _ => None,
        };
        found.push(Landing {
            root: live.clone(),
            range: range.clone(),
            spec: Some(spec.clone()),
            verb: if rewritten { "force-push" } else { "push" },
            also: pushed.len() - 1,
            unmade,
        });
    }
    // Every move the line made, not its last: a reset, switch or stash after
    // the commit on the same line hid it (#508). A commit is the line's when
    // it grows from where the line started (or from where its rebase ended),
    // or amends a commit the line started on or made: another session's
    // commit after a reset of the same checkout does neither (#406).
    let mut anchor = before.head.clone();
    let mut ours: Vec<String> = vec![before.head.clone()];
    let mut start: Option<String> = None;
    let mut last: Option<(String, &'static str)> = None;
    // A rebase replays onto the commit its `(start)` entry checks out, one
    // `(pick)` per commit, and `(finish)` moves the branch to the last.
    let mut onto: Option<String> = None;
    let mut tip: Option<String> = None;
    for entry in &entries? {
        let (old, new) = (entry.old.to_string(), entry.new.to_string());
        let action = entry.action.as_str();
        if is_rebase_step(action) {
            if action.ends_with("(start)") {
                onto = Some(new);
                tip = None;
            } else if action.ends_with("(abort)") {
                onto = None;
                tip = None;
            } else if action.ends_with("(finish)") {
                let (Some(base), Some(end)) = (onto.take(), tip.take()) else {
                    continue;
                };
                if base == end || claimed_by_another(&end, session) {
                    continue;
                }
                start = Some(base);
                anchor.clone_from(&end);
                last = Some((end, "rebase"));
            } else {
                ours.push(new.clone());
                tip = Some(new);
            }
            continue;
        }
        let Some(verb) = commit_move(action) else {
            continue;
        };
        // Catching up with the branch's own upstream brings in commits that
        // already landed there: nothing for this session to review.
        if verb == "merge"
            && entry.subject.contains("Fast-forward")
            && snap::rev_oid(root, "@{upstream}")
                .map_err(|_| "error:git")?
                .as_deref()
                == Some(new.as_str())
        {
            continue;
        }
        if claimed_by_another(&new, session) {
            continue;
        }
        let amend = action == "commit (amend)";
        let grows = if amend {
            ours.contains(&old)
        } else {
            anchor.is_empty() || ancestor_of(&repo, &anchor, &new)?
        };
        if !grows {
            continue;
        }
        if start.is_none() {
            // A commit with no parent, or none before the line, starts from
            // the empty tree.
            let parent = if amend {
                git_checked(
                    root,
                    &["rev-parse", "--verify", "--quiet", &format!("{new}~1")],
                )?
            } else if before.head.is_empty() {
                None
            } else {
                Some(before.head.clone())
            };
            start = Some(match parent {
                Some(parent) => parent,
                None => snap::empty_tree_id(root).map_err(|_| "error:git")?,
            });
        }
        ours.push(new.clone());
        last = Some((new, verb));
    }
    let (Some((end, verb)), Some(start)) = (last, start) else {
        return Ok(found);
    };
    for oid in &ours[1..] {
        claim(oid, session);
    }
    // A push of the commit the line made reviews it already.
    if let Some(tip) = &published {
        if ancestor_of(&repo, &end, tip)? {
            return Ok(found);
        }
    }
    found.push(Landing {
        root: live,
        range: format!("{start}..{end}"),
        spec: None,
        verb,
        also: 0,
        unmade: Vec::new(),
    });
    Ok(found)
}

/// A commit a session's hook took as its line's is no other session's: the
/// marker names the session, so a background line of another one that could
/// have written it does not claim it again (Forgejo #97).
fn claim(oid: &str, session: Option<&str>) {
    let session = file_safe(session);
    if !session.is_empty() {
        mark_seen_with(&format!("claim-{oid}"), &session);
    }
}

fn claimed_by_another(oid: &str, session: Option<&str>) -> bool {
    let session = file_safe(session);
    !session.is_empty()
        && seen_headline(&format!("claim-{oid}"), SEEN_TTL)
            .is_some_and(|by| !by.is_empty() && by != session)
}

fn claimed_by(oid: &str, session: &str) -> bool {
    seen_headline(&format!("claim-{oid}"), SEEN_TTL).is_some_and(|by| by == session)
}

/// The commits of `range` that this call did not commit (its reflog entries
/// say which it did) and that no earlier call of this session's hook took as
/// its own: what a push over another session's held-back commit publishes
/// unannounced (gh#669). The commits this call made are claimed here, so the
/// next push does not read them as foreign. Nothing when the session is
/// unknown: a warning needs its evidence. A range git cannot list is a
/// refusal, not a push with nothing foreign in it.
fn unmade_commits(
    root: &Path,
    range: &str,
    entries: &[snap::ReflogEntry],
    session: Option<&str>,
) -> Result<Vec<String>, Silence> {
    let session = file_safe(session);
    if session.is_empty() {
        return Ok(Vec::new());
    }
    let made: rustc_hash::FxHashSet<String> = entries
        .iter()
        .filter(|e| commit_move(&e.action).is_some() || is_rebase_step(&e.action))
        .map(|e| e.new.to_string())
        .collect();
    for oid in &made {
        claim(oid, Some(session.as_str()));
    }
    let Some(listing) = git_checked(
        root,
        &["log", "-n", "500", "--format=%H %h %s", range, "--"],
    )?
    else {
        return Ok(Vec::new());
    };
    Ok(listing
        .lines()
        .filter_map(|line| {
            let (oid, rest) = line.split_once(' ')?;
            (!made.contains(oid) && !claimed_by(oid, &session)).then(|| {
                let (short, subject) = rest.split_once(' ').unwrap_or((rest, ""));
                let subject: String = subject.chars().take(60).collect();
                format!("{short} '{subject}'")
            })
        })
        .collect())
}

/// What a line printed, as its `PostToolUse` carries it: git's report of
/// each ref a push touched, `old..new  from -> to` (no range for a new
/// branch or a rejection). `None` when the payload holds no output — a line
/// still running in the background, or one that printed nothing, which a
/// quiet push does — and the remote-tracking refs' reflogs say instead.
struct Output {
    updates: Option<Vec<Update>>,
}

struct Update {
    /// The ref's `old..new`, abbreviated as git prints them.
    range: Option<(String, String)>,
    /// The ref on the remote: a branch (`main`), or for a fetch's report the
    /// tracking ref it updated (`origin/main`).
    to: String,
}

impl Output {
    fn none() -> Self {
        Self { updates: None }
    }

    fn of(payload: &serde_json::Value) -> Self {
        let response = &payload["tool_response"];
        let text = match response {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Object(_) if !in_background(payload) => {
                let parts: Vec<&str> = ["stdout", "stderr"]
                    .iter()
                    .filter_map(|k| response[*k].as_str())
                    .collect();
                (!parts.is_empty()).then(|| parts.join("\n"))
            }
            _ => None,
        };
        Self {
            updates: text
                .filter(|t| !t.trim().is_empty())
                .map(|t| t.lines().filter_map(update_of).collect()),
        }
    }
}

/// `   ae8a63af7..d4ce1daad  main -> main`, the forced `+ a...b`, or the
/// `* [new branch]  topic -> topic` that has no range.
fn update_of(line: &str) -> Option<Update> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let arrow = words.iter().position(|w| *w == "->")?;
    let to = words.get(arrow + 1)?.to_string();
    let hex = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit());
    let range = words[..arrow]
        .iter()
        .find_map(|w| w.split_once("...").or_else(|| w.split_once("..")))
        .filter(|(old, new)| hex(old) && hex(new))
        .map(|(old, new)| (old.to_string(), new.to_string()));
    Some(Update { range, to })
}

/// The range a push moved a ref over, named and as the engine opens it.
/// When `old` is not an ancestor of `new` the ref was rewritten, and what
/// the push changed on the remote is the tree delta between the two tips,
/// which a range name would read as `merge-base..new` (gh#596): it is a
/// `Rewritten` value, and its name ends in the new tip's tree.
fn push_range(
    repo: &snap::RepoRoot,
    old: &str,
    new: &str,
) -> Result<(String, snap::RangeSpec), Silence> {
    let (Ok(from), Ok(to)) = (snap::Oid::parse(old), snap::Oid::parse(new)) else {
        return Err("error:git");
    };
    if ancestor_of(repo, old, new)? {
        return Ok((
            format!("{old}..{new}"),
            snap::RangeSpec::Pushed { old: from, new: to },
        ));
    }
    let tree = git_checked(
        &repo.work_tree,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{new}^{{tree}}"),
        ],
    )?
    .ok_or("error:git")?;
    Ok((
        format!("{old}..{tree}"),
        snap::RangeSpec::Rewritten { old: from, new: to },
    ))
}

/// Whether one of `trees` has `branch` checked out. A work tree the line
/// removed has nothing checked out; a detached `HEAD` is git's quiet "no".
fn checked_out_in(trees: &[PathBuf], branch: &str) -> Result<bool, Silence> {
    for tree in trees.iter().filter(|tree| tree.is_dir()) {
        let head = git_checked(tree, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
        if head.as_deref() == Some(branch) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The ranges this line pushed. What git printed for it is exact: no ref
/// that moved meanwhile, no stale tracking ref of a second alias, no peer's
/// push says otherwise. A push line is told from a fetch's by its target
/// being a branch some remote has (`main`), not a tracking ref (`origin/main`).
/// A branch the push created has no range printed: its tracking ref's
/// reflog says. Without output (a line in the background, or one that
/// printed nothing), the first push the remote-tracking refs record since
/// the line started; a line that printed no push line pushed nothing,
/// whatever a peer moved (Forgejo #121).
fn pushed_ranges(
    repo: &snap::RepoRoot,
    before: &Before,
    now: &Before,
    output: &Output,
    others: &[PathBuf],
) -> Result<Vec<(String, snap::RangeSpec)>, Silence> {
    let root = repo.work_tree.as_path();
    let started = |name: &str| before.tracking.iter().find(|t| t.name == name);
    let Some(updates) = output.updates.as_ref() else {
        let mut ranges = Vec::new();
        for moved in now
            .tracking
            .iter()
            .filter(|t| started(&t.name).map(|s| &s.oid) != Some(&t.oid))
        {
            // The reflog of a remote-tracking ref is shared by every work
            // tree and names none: a branch another work tree has checked
            // out was pushed from there, not by this call.
            if checked_out_in(others, &moved.branch)? {
                continue;
            }
            if let Some(range) = first_push_since(repo, before, now, moved)? {
                ranges.push(range);
            }
        }
        return Ok(ranges);
    };
    let pushed_to = |branch: &str| {
        updates
            .iter()
            .any(|u| u.to == branch && now.tracking.iter().any(|t| t.branch == branch))
    };
    let mut ranges = Vec::new();
    for (old, new) in updates
        .iter()
        .filter_map(|u| u.range.as_ref().filter(|_| pushed_to(&u.to)))
    {
        // A tip this clone does not hold names no range to review; a read git
        // could not answer is the hook's error.
        let held = |rev: &str| snap::rev_oid(root, rev).map_err(|_| "error:git");
        if let (Some(old), Some(new)) = (held(old)?, held(new)?) {
            ranges.push(push_range(repo, &old, &new)?);
        }
    }
    // Created by this line: not a ref the line started with.
    for created in now
        .tracking
        .iter()
        .filter(|t| started(&t.name).is_none() && pushed_to(&t.branch))
    {
        if let Some(range) = first_push_since(repo, before, now, created)? {
            ranges.push(range);
        }
    }
    Ok(ranges)
}

/// The first push `moved`'s reflog records after the value the line started
/// from, as the range it moved the ref over. A branch the push created is
/// measured from what the remote's other refs already hold (fj#127): the
/// commits the new tip has that none of them has.
fn first_push_since(
    repo: &snap::RepoRoot,
    before: &Before,
    now: &Before,
    moved: &Tracked,
) -> Result<Option<(String, snap::RangeSpec)>, Silence> {
    let from = before
        .tracking
        .iter()
        .find(|t| t.name == moved.name)
        .map_or(0, |t| t.log);
    let (entries, _) = snap::reflog_since(
        repo,
        &snap::RefName(moved.name.clone()),
        &snap::Cursor(from),
    )
    .map_err(|_| "error:git")?;
    let Some(push) = entries
        .iter()
        .find(|e| e.action.starts_with("update by push"))
    else {
        return Ok(None);
    };
    let pushed = push.new.to_string();
    if !push.old.is_zero() {
        return push_range(repo, &push.old.to_string(), &pushed).map(Some);
    }
    let root = repo.work_tree.as_path();
    let others: Vec<&str> = now
        .tracking
        .iter()
        .filter(|t| t.remote == moved.remote && t.name != moved.name)
        .map(|t| t.oid.as_str())
        .take(500)
        .collect();
    if others.is_empty() {
        return Ok(None);
    }
    let mut args = vec!["rev-list", "--reverse", pushed.as_str(), "--not"];
    args.extend(others);
    // The oldest commit the remote did not have: its parent is the base.
    let Some(oldest) = git_checked(root, &args)?.and_then(|l| l.lines().next().map(str::to_string))
    else {
        return Ok(None);
    };
    let parent = git_checked(
        root,
        &["rev-parse", "--verify", "--quiet", &format!("{oldest}^")],
    )?;
    let base = match parent.clone() {
        Some(parent) => parent,
        None => snap::empty_tree_id(root).map_err(|_| "error:git")?,
    };
    let (Ok(old), Ok(new)) = (snap::Oid::parse(&base), snap::Oid::parse(&pushed)) else {
        return Err("error:git");
    };
    // A branch whose first commit is a root starts from the empty tree,
    // which is no commit to descend from.
    let spec = if parent.is_some() {
        snap::RangeSpec::Pushed { old, new }
    } else {
        snap::RangeSpec::Review {
            base: old,
            head: new,
        }
    };
    Ok(Some((format!("{base}..{pushed}"), spec)))
}

/// FNV-1a, 64-bit: no dependency, and no collision that matters at the
/// scale of one user's markers.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn feed(&mut self, bytes: &[u8]) -> &mut Self {
        for b in bytes.iter().chain([&0u8]) {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self
    }
}

/// The identity of the answer a range would get: the analyser's version,
/// the checkout, the base commit, and every path the range changes with the
/// mode and blob id it ends at — content-addressed, so every spelling of one
/// snapshot shares it: the worktree with an untracked file, the index after
/// `git add -A`, the commit that records it. Any byte of the change, its
/// base or the analyser changing makes a new one. `Ok(None)` when the range
/// changes nothing; a read git refused is an error, never "nothing": an
/// unknown set of files changed.
pub fn result_identity(root: &Path, range: &str) -> Result<Option<String>, Silence> {
    identity_of(root, range).map(|(identity, _)| identity)
}

/// `result_identity` and, for the working tree, the untracked directories
/// its collapsed read left unread (a budget of entries bounds it, so a
/// checkout of a million untracked files costs what its top levels cost):
/// they are in the identity by name, and the review says it did not read them.
fn identity_of(root: &Path, range: &str) -> Result<(Option<String>, Vec<String>), Silence> {
    let (base, head) = match snap::split_diff_range(range) {
        (Some(b), h) => (b, h),
        _ => (range.to_string(), None),
    };
    let mut entries = changed_blobs(root, &base, head.as_deref())?;
    let mut unread = Vec::new();
    if head.is_none() {
        let untracked = snap::untracked(root, &[]).map_err(|_| "error:git")?;
        entries.extend(
            untracked
                .names
                .into_iter()
                .map(|p| (p, "100644".to_string(), String::new())),
        );
        entries.extend(untracked.unread.iter().map(|dir| {
            (
                format!("{dir}/"),
                "040000".to_string(),
                "unread".to_string(),
            )
        }));
        unread = untracked.unread;
    }
    if entries.is_empty() {
        return Ok((None, unread));
    }
    hash_working_tree_blobs(root, &mut entries);
    entries.sort();
    // The checkout by its canonical root: `.` from a manual run and the
    // absolute path the hook resolves are one repository. A path with no work
    // tree is its own.
    let checkout = snap::find_toplevel(root)
        .map_err(|_| "error:git")?
        .unwrap_or_else(|| root.to_path_buf());
    let base =
        git_checked(root, &["rev-parse", "--verify", "--quiet", &base])?.ok_or("error:git")?;
    let mut h = Fnv::new();
    h.feed(env!("CARGO_PKG_VERSION").as_bytes())
        .feed(checkout.to_string_lossy().as_bytes())
        .feed(base.as_bytes());
    for (path, mode, oid) in &entries {
        h.feed(path.as_bytes())
            .feed(mode.as_bytes())
            .feed(oid.as_bytes());
    }
    Ok((Some(format!("{:016x}", h.0)), unread))
}

/// `(path, mode, blob id)` of every path `base..head` changes (`head` absent:
/// the working tree, whose blob ids git leaves as zeros), `deleted` for a
/// path the range removes.
fn changed_blobs(
    root: &Path,
    base: &str,
    head: Option<&str>,
) -> Result<Vec<(String, String, String)>, Silence> {
    let mut args = vec!["diff", "--raw", "-z", "--no-abbrev", "--no-renames", base];
    args.extend(head);
    args.push("--");
    let raw = snap::run_git_z(root, &args).map_err(|_| "error:git")?;
    Ok(raw
        .chunks(2)
        .filter_map(|pair| {
            let [meta, path] = pair else { return None };
            let fields: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
            let [_, mode, _, oid, status] = fields.as_slice() else {
                return None;
            };
            let oid = if status.starts_with('D') {
                "deleted"
            } else {
                oid
            };
            Some((path.clone(), (*mode).to_string(), oid.to_string()))
        })
        .collect())
}

/// Fills the blob ids the working tree leaves open (zeros, untracked files)
/// with what `git add` would store. What git cannot hash — a nested
/// repository, a moved submodule, an unreadable file — keeps an identity of
/// its own: one such path used to fail the whole read, which every caller
/// logs as "clean", and the hook went quiet on a real change.
#[allow(
    clippy::disallowed_methods,
    reason = "one batch `hash-object` git refuses leaves each path its own stat identity (above), which only costs a repeated review"
)]
fn hash_working_tree_blobs(root: &Path, entries: &mut [(String, String, String)]) {
    let open = |oid: &str| oid.is_empty() || oid.bytes().all(|b| b == b'0');
    let files: Vec<String> = entries
        .iter()
        .filter(|(p, mode, oid)| open(oid) && mode != "160000" && root.join(p).is_file())
        .map(|(p, _, _)| p.clone())
        .collect();
    let hashed: FxHashMap<String, String> = snap::hash_object_paths(root, &files)
        .map(|oids| files.iter().cloned().zip(oids).collect())
        .unwrap_or_default();
    for (path, _, oid) in entries.iter_mut().filter(|(_, _, oid)| open(oid)) {
        *oid = hashed
            .get(path.as_str())
            .cloned()
            .unwrap_or_else(|| unhashable_identity(&root.join(&*path)));
    }
}

/// A checkout's commit for a repository inside this one, else size and mtime.
#[allow(
    clippy::disallowed_methods,
    reason = "an identity of its own: a path that cannot be stat'ed is `missing`, a time the platform does not give is 0; both only repeat a review"
)]
fn unhashable_identity(path: &Path) -> String {
    if path.is_dir() {
        if let Ok(Some(head)) = git_checked(path, &["rev-parse", "--verify", "--quiet", "HEAD"]) {
            return format!("head-{head}");
        }
    }
    std::fs::metadata(path).map_or_else(
        |_| "missing".to_string(),
        |m| {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("stat-{}-{mtime}", m.len())
        },
    )
}

/// Where the hook keeps what it delivered and its log. A cache it cannot
/// write disabled the dedupe and the retry cap without a word, and every
/// commit reran the pipeline (#407): the temporary directory stands in.
#[allow(
    clippy::disallowed_methods,
    reason = "no `USER` (Windows) leaves the fallback directory unsuffixed"
)]
fn cache_root() -> Option<PathBuf> {
    let preferred = configured_cache_root();
    if let Some(dir) = preferred.as_ref().filter(|d| writable(d)) {
        return Some(dir.clone());
    }
    let user = std::env::var("USER").unwrap_or_default();
    let fallback = std::env::temp_dir().join(format!("diffctx-{user}"));
    writable(&fallback).then_some(fallback)
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".probe-{}", std::process::id()));
    std::fs::create_dir_all(dir).is_ok()
        && std::fs::write(&probe, b"").is_ok()
        && std::fs::remove_file(&probe).is_ok()
}

fn configured_cache_root() -> Option<PathBuf> {
    let base = std::env::var_os("DIFFCTX_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from))
        .or_else(|| {
            if cfg!(windows) {
                std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
            } else {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache"))
            }
        })?;
    Some(base.join("diffctx"))
}

fn cache_dir() -> Option<PathBuf> {
    Some(cache_root()?.join("seen"))
}

fn marker(key: &str) -> Option<PathBuf> {
    Some(cache_dir()?.join(key))
}

pub fn is_seen(key: &str) -> bool {
    is_seen_within(key, SEEN_TTL)
}

#[allow(
    clippy::disallowed_methods,
    reason = "a marker that cannot be stat'ed is not seen: the review runs again, never silence"
)]
fn is_seen_within(key: &str, ttl: Duration) -> bool {
    let Some(path) = marker(key) else {
        return false;
    };
    let Ok(meta) = std::fs::metadata(&path) else {
        return false;
    };
    meta.modified()
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .is_some_and(|age| age < ttl)
}

/// The hook's own markers are per session: the payload names it, and a
/// change one session was shown is news to the next (#323). Only what a
/// file name can carry is kept of the id.
fn session_key(key: &str, session: Option<&str>) -> String {
    let session = file_safe(session);
    if session.is_empty() {
        key.to_string()
    } else {
        format!("{key}.{session}")
    }
}

fn file_safe(session: Option<&str>) -> String {
    session
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(64)
        .collect()
}

pub fn mark_seen(key: &str) {
    mark_seen_with(key, "");
}

/// The marker holds the answer's headline, so a later commit of the same
/// change can say what was shown instead of saying nothing.
fn mark_seen_with(key: &str, headline: &str) {
    let Some(path) = marker(key) else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Hooks of one session can run at once: a marker is written whole or not
    // at all, never read half-written.
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, headline.as_bytes()).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn unmark(key: &str) {
    if let Some(path) = marker(key) {
        let _ = std::fs::remove_file(path);
    }
}

/// Failed attempts at an answer for this identity. A timeout is never a
/// review: the answer is retried, up to `MAX_ATTEMPTS` runs, then left alone
/// rather than costing the deadline on every command.
const MAX_ATTEMPTS: u32 = 2;

#[allow(
    clippy::disallowed_methods,
    reason = "a marker that is no number counts as no attempt: the review is tried again, never skipped"
)]
fn attempts(key: &str) -> u32 {
    seen_headline(key, SEEN_TTL)
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0)
}

/// A delivered answer: its headline, and whether the run behind it stopped
/// at a limit.
struct Delivered {
    headline: String,
    partial: bool,
}

const PARTIAL_MARK: &str = "partial:";

fn delivered(key: &str, ttl: Duration) -> Option<Delivered> {
    seen_headline(key, ttl).map(|text| match text.strip_prefix(PARTIAL_MARK) {
        Some(rest) => Delivered {
            headline: rest.to_string(),
            partial: true,
        },
        None => Delivered {
            headline: text,
            partial: false,
        },
    })
}

fn mark_delivered(key: &str, headline: &str, partial: bool) {
    let text = if partial {
        format!("{PARTIAL_MARK}{headline}")
    } else {
        headline.to_string()
    };
    mark_seen_with(key, &text);
}

#[allow(
    clippy::disallowed_methods,
    reason = "a marker that cannot be read is no marker: the review runs again, never an answer read as empty"
)]
fn seen_headline(key: &str, ttl: Duration) -> Option<String> {
    if !is_seen_within(key, ttl) {
        return None;
    }
    std::fs::read_to_string(marker(key)?).ok()
}

/// A manual `--mode impact` run on a range counts as that range reviewed:
/// the hooks then stay silent on the commit or push that follows, and the
/// strict gate opens.
pub fn mark_range_reviewed(root: &Path, range: &str, output: &crate::impact::ImpactOutput) {
    if std::env::var_os("DIFFCTX_NO_MARKER").is_some() {
        return;
    }
    if let Ok(Some(identity)) = result_identity(root, range) {
        // An empty answer is stored empty, as the hook's own path does: the
        // reminder then has no callers to point back at.
        let rendered = if output.empty {
            String::new()
        } else {
            crate::impact::render_markdown(output)
        };
        mark_delivered(
            &format!("id-{identity}"),
            rendered.lines().next().unwrap_or(""),
            !output.limits.is_empty(),
        );
    }
}

/// Everything the hook decides before any heavy work: which repository,
/// which range, and what is already known about the answer for it.
struct Prepared {
    root: PathBuf,
    range: String,
    /// The range as the engine opens it, when the hook holds it as a value.
    spec: Option<snap::RangeSpec>,
    trigger: Trigger,
    /// This session's answer for this exact identity, once delivered.
    key: String,
    /// The command path's own marker: the same commit target answered at a
    /// commit, push or merge in this session is silence there.
    record_key: String,
    /// Failed attempts at an answer for this identity in this session.
    tries_key: String,
    /// Delivered earlier for exactly this identity, on an inspection or by a
    /// manual run: a command recording it gets a one-line reminder (#346).
    shown: Option<Delivered>,
    /// Untracked directories of a pending change that the collapsed read of
    /// the checkout left unread: the answer names them.
    unread: Vec<String>,
}

/// What one invocation learned about itself, for its line in the hook log.
#[derive(Default)]
struct Trace {
    session: Option<String>,
    /// The tool call this run belongs to: the join between a call's
    /// `PreToolUse` and `PostToolUse` lines and the session's transcript.
    call: Option<String>,
    /// Every repository the run found something landed in; the log's repo
    /// field names the first.
    roots: Vec<PathBuf>,
    verb: Option<&'static str>,
    root: Option<PathBuf>,
    range: Option<String>,
    /// Why a one-line answer was given in place of a review, for the log.
    noted: Option<Silence>,
}

/// Why the hook stayed silent, as its log line names it; `shown` otherwise.
type Silence = &'static str;

fn verb_of(trigger: &Trigger) -> &'static str {
    match trigger {
        Trigger::Commit { .. } => "commit",
        Trigger::Landed { verb, .. } => verb,
        Trigger::PullRequest { .. } => "pr",
        Trigger::Inspect { .. } => "inspect",
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "no working directory is `not-git` to every caller, which names it"
)]
fn payload_cwd(payload: &serde_json::Value) -> Option<PathBuf> {
    payload["cwd"]
        .as_str()
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
}

/// A relative directory on the command line is relative to where the agent
/// ran it, which the payload names; this process's cwd is a guess.
fn root_dir(payload: &serde_json::Value, dir: Option<&str>) -> Option<PathBuf> {
    let dir = resolve_dir(&payload_cwd(payload)?, dir);
    // A search names a file as often as a directory.
    Some(if dir.is_file() {
        dir.parent()?.to_path_buf()
    } else {
        dir
    })
}

/// The work tree a payload's directory is in: a directory that is no
/// repository is `None`, a read git refused for any other reason is a
/// failure, not "not git".
fn repo_root_checked(
    payload: &serde_json::Value,
    dir: Option<&str>,
) -> Result<Option<PathBuf>, Silence> {
    let Some(dir) = root_dir(payload, dir) else {
        return Ok(None);
    };
    snap::find_toplevel(&dir).map_err(|_| "error:git")
}

fn prepare(
    event: Event,
    payload: &serde_json::Value,
    trace: &mut Trace,
) -> Result<Prepared, Silence> {
    let _phase = crate::profile::phase("hook.prepare");
    let command = payload["tool_input"]["command"]
        .as_str()
        .ok_or("no-trigger")?;
    let intent =
        intent::of(matches!(event, Event::PostToolUse { .. }), command).ok_or("no-trigger")?;
    let mut trigger = match intent {
        Intent::Commit { amend, plain } => Trigger::Commit { amend, plain },
        Intent::PullRequest { base, head } => Trigger::PullRequest { base, head },
        Intent::Inspect { staged } => Trigger::Inspect { staged, own: None },
    };
    trace.verb = Some(verb_of(&trigger));
    let root = repo_root_checked(payload, None)?.ok_or("not-git")?;
    trace.root = Some(root.clone());
    if let Trigger::Inspect { staged, own } = &mut trigger {
        *own = own_changes(&root, payload, *staged)?;
    }
    let range = range_for(&root, &trigger)?;
    prepared(root, range, None, trigger, trace)
}

/// What the line recorded, from the record its `PreToolUse` left: every
/// repository it touched that moved (#608). A line run in the background
/// has its `PostToolUse` at launch, before git records anything, so its
/// record waits for a later event of the session (#617).
fn prepare_landed(
    payload: &serde_json::Value,
    command: &str,
    trace: &mut Trace,
) -> Result<Vec<Prepared>, Silence> {
    let _phase = crate::profile::phase("hook.prepare");
    let key = before_key(payload, command, trace.session.as_deref());
    let text = seen_headline(&key, SEEN_TTL).ok_or("no-record")?;
    let record: Record = serde_json::from_str(&text).map_err(|_| "error:record")?;
    unmark(&key);
    let output = Output::of(payload);
    let found = landed_in(&record, false, &output, trace)?;
    if !found.is_empty() {
        forget_pending(trace.session.as_deref(), &found);
        return Ok(found);
    }
    if in_background(payload) {
        keep_pending(record, trace.session.as_deref());
        return Err("pending");
    }
    Err("nothing-landed")
}

/// Before any Bash call runs: where the repository it starts in, and each
/// of that repository's linked work trees, stand, and what the call's own
/// work tree holds by stat. Nothing about what the call will do is read off
/// its text: git's reflog says it afterwards, and whatever moved in these
/// repositories during the call is the call's (a peer committing in the same
/// checkout is indistinguishable from it). Nothing here reads a file's
/// bytes, and git is asked about refs once for the repository, not once per
/// work tree.
fn record_before(
    payload: &serde_json::Value,
    command: &str,
    trace: &mut Trace,
) -> Result<(), Silence> {
    let _phase = crate::profile::phase("hook.record");
    let cwd = payload_cwd(payload).ok_or("not-git")?;
    if !cwd.is_dir() {
        return Err("not-git");
    }
    let home = match snap::open(&cwd) {
        Ok(repo) => repo,
        Err(snap::SnapshotError::NotARepo(_)) => return Err("not-git"),
        Err(_) => return Err("error:git"),
    };
    // The stat listing is one git call of its own: it runs beside the refs.
    let repos = std::thread::scope(|scope| {
        let stamped = scope.spawn(crate::profile::carry(|| {
            bracket_before(payload, &home.work_tree);
        }));
        let repos = repos_before(&home);
        let _ = stamped.join();
        repos
    })?;
    trace.root = Some(home.work_tree.clone());
    let record = Record {
        repos,
        at: now_secs(),
    };
    let json = serde_json::to_string(&record).map_err(|_| "error:record")?;
    mark_seen_with(
        &before_key(payload, command, trace.session.as_deref()),
        &json,
    );
    Ok(())
}

/// `home` and its linked work trees as the call finds them. The remote-tracking
/// refs are shared by every work tree, so only `home`'s record holds them; the
/// others hold where their own `HEAD` stands.
fn repos_before(home: &snap::RepoRoot) -> Result<Vec<Before>, Silence> {
    let mut repos = vec![refs_of(home)?];
    for (work_tree, head) in work_trees(home)? {
        if work_tree == home.work_tree || !work_tree.is_dir() {
            continue;
        }
        let log = head_log(&work_tree)?;
        repos.push(Before {
            root: work_tree,
            head,
            tracking: Vec::new(),
            head_cursor: log_len(&log)?,
            head_log: Some(log),
            common: Some(home.common_dir.clone()),
        });
    }
    Ok(repos)
}

/// Every repository of `record` that moved, as a range to review each.
fn landed_in(
    record: &Record,
    late: bool,
    output: &Output,
    trace: &mut Trace,
) -> Result<Vec<Prepared>, Silence> {
    let mut found = Vec::new();
    let mut ranges = Vec::new();
    let mut unreadable = None;
    for (i, before) in record.repos.iter().enumerate() {
        // The first repository of a record is the one the call started in.
        let others: Vec<PathBuf> = record
            .repos
            .iter()
            .filter(|b| b.root != before.root)
            .map(|b| b.root.clone())
            .collect();
        let pushes = (i == 0).then_some(others.as_slice());
        let landings = match landed(before, output, trace.session.as_deref(), pushes) {
            Ok(landings) => landings,
            Err(why) => {
                unreadable = unreadable.or(Some(why));
                continue;
            }
        };
        for landing in landings {
            let Landing {
                root,
                range,
                spec,
                verb,
                also,
                unmade,
            } = landing;
            if found.is_empty() {
                trace.verb = Some(verb);
                trace.root = Some(root.clone());
            }
            if !trace.roots.contains(&root) {
                trace.roots.push(root.clone());
            }
            ranges.push(range.clone());
            let trigger = Trigger::Landed {
                verb,
                also,
                late,
                unmade,
            };
            match prepared(root, range, spec, trigger, trace) {
                Ok(p) => found.push(p),
                Err(why) => unreadable = unreadable.or(Some(why)),
            }
        }
    }
    if !ranges.is_empty() {
        trace.range = Some(ranges.join(","));
    }
    match unreadable {
        Some(why) if found.is_empty() => Err(why),
        _ => Ok(found),
    }
}

fn in_background(payload: &serde_json::Value) -> bool {
    payload["tool_input"]["run_in_background"].as_bool() == Some(true)
        || payload["tool_response"]["backgroundTaskId"].is_string()
}

/// How long a background line's record waits to land: a pre-commit that
/// runs the whole suite takes minutes, and a record older than this would
/// claim whatever lands next in that repository.
const PENDING_TTL: Duration = Duration::from_secs(2 * 60 * 60);

fn pending_key(session: Option<&str>) -> String {
    session_key("pending", session)
}

#[allow(
    clippy::disallowed_methods,
    reason = "a pending record this hook wrote and cannot parse is dropped: one background line goes unreviewed, no answer is wrong"
)]
fn pending(session: Option<&str>) -> Vec<Record> {
    seen_headline(&pending_key(session), PENDING_TTL)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn store_pending(session: Option<&str>, records: &[Record]) {
    match serde_json::to_string(records) {
        Ok(json) if !records.is_empty() => mark_seen_with(&pending_key(session), &json),
        _ => unmark(&pending_key(session)),
    }
}

fn keep_pending(record: Record, session: Option<&str>) {
    let mut records = pending(session);
    records.push(record);
    store_pending(session, &records);
}

/// A repository this session's own line just reviewed: an earlier
/// background record there would claim that commit as its own.
fn forget_pending(session: Option<&str>, reviewed: &[Prepared]) {
    let mut records = pending(session);
    let before = records.len();
    records.retain(|r| {
        !r.repos
            .iter()
            .any(|b| reviewed.iter().any(|p| p.root == b.root))
    });
    if records.len() != before {
        store_pending(session, &records);
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What a background line of this session recorded since an earlier event
/// left its record waiting, answered at whatever event comes next, once.
fn pending_review(event: Event, session: Option<&str>, started: Instant) -> Option<String> {
    let records = pending(session);
    if records.is_empty() {
        return None;
    }
    let mut trace = Trace {
        session: session.map(str::to_string),
        ..Trace::default()
    };
    let mut waiting = Vec::new();
    let mut answers = Vec::new();
    let mut reviewed = false;
    for record in records {
        match landed_in(&record, true, &Output::none(), &mut trace) {
            Ok(found) if !found.is_empty() => {
                reviewed = true;
                for p in found {
                    if let Ok(a) = answer(event, p, false, started, &mut trace) {
                        answers.push(a);
                    }
                }
            }
            _ if now_secs().saturating_sub(record.at) < PENDING_TTL.as_secs() => {
                waiting.push(record);
            }
            _ => {}
        }
    }
    store_pending(session, &waiting);
    if reviewed {
        log_outcome(event, &trace, "late", started.elapsed());
    }
    joined(answers)
}

/// Several answers of one event as one hook output: the last one's (a
/// gate's decision stays), with every earlier one's context before its own.
#[allow(
    clippy::disallowed_methods,
    reason = "every answer is `hook_json`'s own output: one that carries no context adds none"
)]
fn joined(answers: Vec<String>) -> Option<String> {
    let mut answers = answers.into_iter().rev();
    let last = answers.next()?;
    Some(answers.fold(last, |acc, earlier| {
        let context = serde_json::from_str::<serde_json::Value>(&earlier)
            .ok()
            .and_then(|d| {
                d["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .map(str::to_string)
            })
            .unwrap_or_default();
        with_notice(&acc, &context)
    }))
}

fn prepared(
    root: PathBuf,
    range: String,
    spec: Option<snap::RangeSpec>,
    trigger: Trigger,
    trace: &mut Trace,
) -> Result<Prepared, Silence> {
    trace.range = Some(range.clone());
    let (identity, unread) = identity_of(&root, &range)?;
    let identity = identity.ok_or("clean")?;
    let session = trace.session.as_deref();
    let key = session_key(&format!("id-{identity}"), session);
    let shown =
        delivered(&key, SEEN_TTL).or_else(|| delivered(&format!("id-{identity}"), MANUAL_SEEN_TTL));
    Ok(Prepared {
        root,
        range,
        spec,
        trigger,
        record_key: session_key(&format!("rec-{identity}"), session),
        tries_key: session_key(&format!("try-{identity}"), session),
        key,
        shown,
        unread,
    })
}

/// Bytes of transcript read to tell this session's edits apart. A long
/// session writes tens of megabytes; past this the answer says it cannot
/// tell rather than miss an edit made early on.
const TRANSCRIPT_BUDGET: u64 = 256 * 1024 * 1024;

const EDIT_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// Which of the changed files an inspection shows this session wrote, by
/// its own transcript; `None` when no transcript can tell. Another session's
/// uncommitted work in a shared checkout is not this one's pending change:
/// a session that wrote none of the changed files has nothing to review
/// (#404).
fn own_changes(
    root: &Path,
    payload: &serde_json::Value,
    staged: bool,
) -> Result<Option<Owned>, Silence> {
    let changed = changed_paths(root, staged)?;
    if changed.is_empty() {
        return Err("clean");
    }
    let ledger = bracketed(payload["session_id"].as_str(), root);
    let Some(writes) = payload["transcript_path"]
        .as_str()
        .and_then(|t| session_writes(Path::new(t), ledger))
    else {
        return Ok(None);
    };
    let top = canonical(root);
    let (files, others): (Vec<String>, Vec<String>) =
        changed.into_iter().partition(|p| writes.wrote(&top, p));
    if files.is_empty() {
        return Err(if writes.unbracketed > 0 {
            "unbracketed"
        } else {
            "not-this-session"
        });
    }
    let prior = files
        .iter()
        .filter(|p| writes.ledger.prior.contains(*p) && !writes.files.contains(&top.join(p)))
        .cloned()
        .collect();
    Ok(Some(Owned {
        files,
        others,
        prior,
        unbracketed: writes.unbracketed,
    }))
}

/// Every path the inspected snapshot changes: the index against `HEAD`, or
/// the working tree with its untracked files, read collapsed: a directory
/// the read did not reach is one entry with a trailing `/`. A listing git
/// refused is an error, not a smaller change.
fn changed_paths(root: &Path, staged: bool) -> Result<Vec<String>, Silence> {
    let args: &[&str] = if staged {
        &["diff", "--cached", "--name-only", "-z", "--no-renames"]
    } else {
        &["diff", "--name-only", "-z", "--no-renames", "HEAD", "--"]
    };
    let mut paths = snap::run_git_z(root, args).map_err(|_| "error:git")?;
    if !staged {
        let untracked = snap::untracked(root, &[]).map_err(|_| "error:git")?;
        paths.extend(untracked.names);
        paths.extend(untracked.unread.into_iter().map(|dir| format!("{dir}/")));
    }
    Ok(paths)
}

/// What a session wrote, by its transcripts and by the calls the hook
/// bracketed: the files its edit tools named, what its Bash calls changed
/// (#453, #662), and how many Bash calls it could not bracket.
#[derive(Default)]
struct Writes {
    files: rustc_hash::FxHashSet<PathBuf>,
    /// Bash calls of the session with no bracket: the hook did not see what
    /// they changed, and a transcript's times do not say it (a call's span
    /// also holds the wait before it ran and a peer's write inside it).
    unbracketed: usize,
    ledger: Bracketed,
}

impl Writes {
    /// An edit tool named the file, or a bracketed Bash call changed it. A
    /// file only an unbracketed call could have written is nobody's: it is
    /// read as committed and the call is counted (#404).
    fn wrote(&self, root: &Path, rel: &str) -> bool {
        self.ledger.paths.contains(rel)
            || self
                .ledger
                .dirs
                .iter()
                .any(|dir| rel.starts_with(dir.as_str()))
            || self.files.contains(&root.join(rel))
            || (rel.ends_with('/') && self.files.iter().any(|f| f.starts_with(root.join(rel))))
    }

    /// One transcript entry: an edit tool's path, a Bash call starting, or
    /// the result that ends one.
    fn note(&mut self, entry: &serde_json::Value, running: &mut rustc_hash::FxHashSet<String>) {
        for item in entry["message"]["content"].as_array().into_iter().flatten() {
            match (item["type"].as_str(), item["name"].as_str()) {
                (Some("tool_use"), Some("Bash")) => {
                    if let Some(id) = item["id"].as_str() {
                        running.insert(id.to_string());
                    }
                }
                (Some("tool_use"), Some(tool)) if EDIT_TOOLS.contains(&tool) => {
                    let input = &item["input"];
                    if let Some(path) = input["file_path"]
                        .as_str()
                        .or_else(|| input["notebook_path"].as_str())
                    {
                        self.files.insert(canonical(Path::new(path)));
                    }
                }
                (Some("tool_result"), _) => {
                    let id = item["tool_use_id"].as_str();
                    if id.is_some_and(|id| running.remove(id))
                        && !id.is_some_and(|id| self.ledger.calls.contains(id))
                    {
                        self.unbracketed += 1;
                    }
                }
                _ => {}
            }
        }
    }
}

/// What the hook saw each Bash call of a session change in a checkout: its
/// `PreToolUse` and `PostToolUse` both stamp the paths that differ from
/// `HEAD` (size and mtime), and the call changed what differs between them.
/// A transcript's times cannot say it: a call's span also holds the wait
/// before it ran, a peer's write inside it, a later rewrite moves a file out
/// of it, and the call being answered has no result yet (#662, #674,
/// Forgejo #117, #93). A call that could not be bracketed is counted and
/// named, never guessed at.
#[derive(Default)]
struct Bracketed {
    /// Every call the session bracketed, in any repository.
    calls: rustc_hash::FxHashSet<String>,
    /// The paths they changed in this checkout, relative to it. An untracked
    /// directory is listed whole, with a trailing `/`.
    paths: rustc_hash::FxHashSet<String>,
    /// Of those, the untracked directories, which name every file below.
    dirs: Vec<String>,
    /// The paths only calls that found them already modified changed: what
    /// differs from `HEAD` there is more than those calls did.
    prior: rustc_hash::FxHashSet<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct BracketedCall {
    id: String,
    root: PathBuf,
    paths: Vec<String>,
    /// Of `paths`, those already modified (or untracked) when the call began.
    #[serde(default)]
    dirty: Vec<String>,
}

/// More dirty paths than this and a call is left to its transcript span: the
/// two listings would cost more than the attribution is worth.
const MAX_BRACKETED_PATHS: usize = 20_000;

/// Every path the index or working tree holds differently from `HEAD`, with
/// its stamp: size and mtime (0: nothing there). Nothing is hashed, so a
/// rewrite of the same bytes counts as a change, and a path git lists is
/// stamped whatever its kind. An untracked directory is one entry, stamped as
/// the directory: a file added to it or removed from it moves the stamp, a
/// file of it rewritten in place does not. Listing every untracked file
/// instead cost about as much again as the rest of the call's recording on a
/// work tree with thousands of them.
#[allow(
    clippy::disallowed_methods,
    reason = "None is a call that cannot be bracketed: `own_changes` counts it and the answer names it"
)]
fn dirty_stamps(root: &Path) -> Option<FxHashMap<String, u64>> {
    let status = snap::run_git_z(
        root,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain",
            "-z",
            "--untracked-files=normal",
            "--no-renames",
        ],
    )
    .ok()?;
    if status.len() > MAX_BRACKETED_PATHS {
        return None;
    }
    Some(
        status
            .iter()
            .filter_map(|entry| entry.get(3..))
            .map(|path| (path.to_string(), stamp(&root.join(path))))
            .collect(),
    )
}

#[allow(
    clippy::disallowed_methods,
    reason = "0 is the stamp of nothing there, and of a time the platform does not give"
)]
fn stamp(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    Fnv::new()
        .feed(&meta.len().to_le_bytes())
        .feed(&mtime.to_le_bytes())
        .0
        | 1
}

fn bracket_key(session: &str, id: &str) -> String {
    session_key(&format!("bracket-{}", file_safe(Some(id))), Some(session))
}

fn ledger_path(session: &str) -> Option<PathBuf> {
    marker(&session_key("wrote", Some(session)))
}

/// The session and call a Bash event belongs to; a call with no id cannot be
/// bracketed.
fn bracket_ids(payload: &serde_json::Value) -> Option<(&str, &str)> {
    if payload["tool_name"].as_str() != Some("Bash") {
        return None;
    }
    Some((
        payload["session_id"].as_str()?,
        payload["tool_use_id"].as_str()?,
    ))
}

/// The `PreToolUse` of a Bash call leaves what its checkout holds.
fn bracket_before(payload: &serde_json::Value, root: &Path) {
    let _phase = crate::profile::phase("hook.bracket");
    let Some((session, id)) = bracket_ids(payload) else {
        return;
    };
    if let Some(stamps) = dirty_stamps(root) {
        if let Ok(json) = serde_json::to_string(&(root, stamps)) {
            mark_seen_with(&bracket_key(session, id), &json);
        }
    }
}

/// The `PostToolUse` of a Bash call adds what its checkout holds differently
/// now to the session's ledger: a path whose stamp moved, one that appeared,
/// and one that left the listing with a stamp moved (restored, deleted; not
/// one a commit or `git add` merely stopped listing). Never an answer: a
/// call that cannot be bracketed is counted as such.
#[allow(
    clippy::disallowed_methods,
    reason = "a bracket that cannot be read leaves its call unbracketed: counted and named, never guessed"
)]
fn bracket_after(payload: &serde_json::Value) {
    let Some((session, id)) = bracket_ids(payload) else {
        return;
    };
    let key = bracket_key(session, id);
    let Some((root, before)) = seen_headline(&key, SEEN_TTL)
        .and_then(|s| serde_json::from_str::<(PathBuf, FxHashMap<String, u64>)>(&s).ok())
    else {
        return;
    };
    unmark(&key);
    let Some(after) = dirty_stamps(&root) else {
        return;
    };
    let mut paths: Vec<String> = after
        .iter()
        .filter(|(path, now)| before.get(*path) != Some(now))
        .map(|(path, _)| path.clone())
        .chain(
            before
                .iter()
                .filter(|(path, was)| {
                    !after.contains_key(*path) && stamp(&root.join(path)) != **was
                })
                .map(|(path, _)| path.clone()),
        )
        .collect();
    paths.sort();
    let dirty = paths
        .iter()
        .filter(|path| before.contains_key(*path))
        .cloned()
        .collect();
    let call = BracketedCall {
        id: id.to_string(),
        root,
        paths,
        dirty,
    };
    if let (Some(path), Ok(mut line)) = (ledger_path(session), serde_json::to_string(&call)) {
        line.push('\n');
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(line.as_bytes()));
    }
}

/// What the session's bracketed calls changed in `root`.
#[allow(
    clippy::disallowed_methods,
    reason = "a ledger or line that cannot be read leaves its calls unbracketed: counted and named, never guessed"
)]
fn bracketed(session: Option<&str>, root: &Path) -> Bracketed {
    let mut ledger = Bracketed::default();
    let Some(text) = session
        .and_then(ledger_path)
        .and_then(|path| std::fs::read_to_string(path).ok())
    else {
        return ledger;
    };
    for call in text
        .lines()
        .filter_map(|line| serde_json::from_str::<BracketedCall>(line).ok())
    {
        if call.root == root {
            let dirty: rustc_hash::FxHashSet<&String> = call.dirty.iter().collect();
            for path in &call.paths {
                if !dirty.contains(path) {
                    ledger.prior.remove(path);
                } else if !ledger.paths.contains(path) {
                    ledger.prior.insert(path.clone());
                }
            }
            ledger
                .dirs
                .extend(call.paths.iter().filter(|p| p.ends_with('/')).cloned());
            ledger.paths.extend(call.paths);
        }
        ledger.calls.insert(call.id);
    }
    ledger
}

/// What this session and its subagents (`<transcript>/subagents/`) wrote,
/// streamed line by line. `None` when the transcripts cannot be read whole
/// within the budget.
#[allow(
    clippy::disallowed_methods,
    reason = "None is a transcript that cannot be read whole: the answer says it cannot tell this session's edits (`Inspect { own: None }`)"
)]
fn session_writes(transcript: &Path, ledger: Bracketed) -> Option<Writes> {
    use std::io::{BufRead as _, Read as _};
    let mut sources = vec![transcript.to_path_buf()];
    match std::fs::read_dir(transcript.with_extension("").join("subagents")) {
        Ok(dir) => {
            for entry in dir {
                let path = entry.ok()?.path();
                if path.extension().is_some_and(|x| x == "jsonl") {
                    sources.push(path);
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return None,
    }
    let mut left = TRANSCRIPT_BUDGET;
    let mut writes = Writes {
        ledger,
        ..Writes::default()
    };
    // Bash calls by id, until their result says when they returned.
    let mut running: rustc_hash::FxHashSet<String> = rustc_hash::FxHashSet::default();
    let mut line = Vec::new();
    for source in &sources {
        let file = std::fs::File::open(source).ok()?;
        let mut reader = std::io::BufReader::new(file.take(left + 1));
        loop {
            line.clear();
            let read = reader.read_until(b'\n', &mut line).ok()?;
            if read == 0 {
                break;
            }
            left = left.checked_sub(read as u64)?;
            let Ok(text) = std::str::from_utf8(&line) else {
                continue;
            };
            let call = text.contains("\"tool_use\"")
                && std::iter::once(&"Bash")
                    .chain(&EDIT_TOOLS)
                    .any(|tool| text.contains(&format!("\"{tool}\"")));
            let result = text.contains("\"tool_result\"")
                && running.iter().any(|id| text.contains(id.as_str()));
            if call || result {
                if let Ok(entry) = serde_json::from_str(text) {
                    writes.note(&entry, &mut running);
                }
            }
        }
    }
    // A call with no result yet is the one being answered, or one still
    // running: bracketed or not, like the rest.
    writes.unbracketed += running
        .iter()
        .filter(|id| !writes.ledger.calls.contains(id.as_str()))
        .count();
    Some(writes)
}

/// A path as the filesystem resolves it (`/var` is `/private/var` on
/// macOS), through its directory when the file itself is gone.
#[allow(
    clippy::disallowed_methods,
    reason = "a path that cannot be resolved stays as written"
)]
fn canonical(path: &Path) -> PathBuf {
    dunce::canonicalize(path)
        .ok()
        .or_else(|| {
            Some(
                dunce::canonicalize(path.parent()?)
                    .ok()?
                    .join(path.file_name()?),
            )
        })
        .unwrap_or_else(|| path.to_path_buf())
}

fn reminder(event: Event, headline: &str) -> String {
    let what = if headline.trim().is_empty() {
        "reviewed earlier".to_string()
    } else {
        format!("shown earlier in this session: {}", headline.trim())
    };
    let records = if event == Event::PreToolUse {
        "records"
    } else {
        "recorded"
    };
    format!(
        "diffctx: this command {records} a change whose impact was {what}. Those callers still apply; confirm they were checked before going on."
    )
}

fn run_impact(
    root: &Path,
    range: &str,
    spec: Option<snap::RangeSpec>,
    secs: u64,
) -> Result<crate::impact::ImpactOutput, Silence> {
    let scoring = ScoringMode::from_str(DEFAULT_SCORING).map_err(|_| "error:scoring")?;
    match spec {
        Some(spec) => {
            crate::pipeline::build_range_impact(root, spec, &[], DEFAULT_PPR_ALPHA, scoring, secs)
        }
        None => crate::pipeline::build_diff_context_impact(
            root,
            Some(range),
            &[],
            DEFAULT_PPR_ALPHA,
            scoring,
            secs,
        ),
    }
    .map_err(|e| failure_reason(&e))
}

/// The engine's own failure, named by what kind it was: the hook says which
/// instead of "failed". The command line ends the same failures with exit
/// code 3 (git), 2 (a range that cannot be resolved) or 1.
fn failure_reason(error: &anyhow::Error) -> Silence {
    match error.downcast_ref::<snap::SnapshotError>() {
        Some(e) if e.is_interruption() => "error:deadline",
        Some(e) if e.is_usage() => "error:range",
        Some(_) => "error:git",
        None => "error:pipeline",
    }
}

struct Reply {
    context: String,
    /// The line a later reminder repeats.
    headline: String,
    /// The run stopped at a limit: delivered as partial, retried later.
    partial: bool,
}

fn lead_of(event: Event, trigger: &Trigger, range: &str) -> String {
    let lead = match (event, trigger) {
        (_, Trigger::Commit { .. }) => "diffctx reviewed the index this commit will record.",
        (
            _,
            Trigger::Landed {
                verb,
                also,
                late,
                unmade,
            },
        ) => {
            let which = if *late {
                "an earlier background"
            } else {
                "this"
            };
            let mut lead = if *verb == "force-push" {
                format!(
                    "diffctx reviewed what this push changed on the remote ({range}): the ref was rewritten, so this is the tree delta from the old tip to the new one, not the whole rewritten commit."
                )
            } else if *also == 0 {
                format!("diffctx reviewed what {which} {verb} recorded.")
            } else {
                format!(
                    "diffctx reviewed the first ref {which} {verb} moved; {also} other pushed ref(s) are not analysed."
                )
            };
            // Git alone cannot tell a commit another session makes in the
            // same checkout meanwhile from this call's own (T19).
            lead.push_str(if *late {
                " It covers what changed in this repository since that call began."
            } else {
                " It covers what changed in this repository during this call."
            });
            if !unmade.is_empty() {
                lead.push_str(&format!(
                    " This push also publishes {} commit(s) this session did not make (none was seen committed by one of its calls): {}. Confirm they are meant to leave.",
                    unmade.len(),
                    some_of(unmade)
                ));
            }
            return lead;
        }
        (Event::PreToolUse, _) => "diffctx reviewed the commits this pull request proposes.",
        (Event::PostToolUse { .. }, Trigger::Inspect { own: None, .. }) => {
            "diffctx reviewed every pending change in this checkout, which you just inspected; without a readable transcript it cannot tell this session's edits from another session's."
        }
        (Event::PostToolUse { .. }, Trigger::Inspect { own: Some(own), .. })
            if !own.others.is_empty() =>
        {
            let mut lead = format!(
                "diffctx reviewed the pending change this session and its subagents made, which you just inspected; {} other changed file(s) show no write by this session or its subagents and are read as committed: {}.",
                own.others.len(),
                some_of(&own.others)
            );
            if own.unbracketed > 0 {
                lead.push_str(&format!(
                    " {} Bash call(s) of this session could not be bracketed, so a file one of them wrote is among those and is not reviewed.",
                    own.unbracketed
                ));
            }
            lead.push_str(&already_modified(own));
            return lead;
        }
        (Event::PostToolUse { .. }, Trigger::Inspect { own: Some(own), .. })
            if !own.prior.is_empty() =>
        {
            return format!(
                "diffctx reviewed the pending change you just inspected.{}",
                already_modified(own)
            );
        }
        (Event::PostToolUse { .. }, _) => "diffctx reviewed the pending change you just inspected.",
    };
    lead.to_string()
}

/// What a call changed in a file that was already modified when it began is
/// not told apart from what was there: git's record holds no earlier copy.
/// The review covers the file's whole difference from `HEAD`, and says so.
fn already_modified(own: &Owned) -> String {
    if own.prior.is_empty() {
        return String::new();
    }
    format!(
        " {} reviewed file(s) were already modified before this call changed them: {}. For those the review covers their whole difference from HEAD, not only what the call changed.",
        own.prior.len(),
        some_of(&own.prior)
    )
}

/// The first few of a list of paths, the rest counted.
fn some_of(paths: &[String]) -> String {
    const SHOWN: usize = 3;
    let shown = paths[..paths.len().min(SHOWN)].join(", ");
    match paths.len().checked_sub(SHOWN) {
        Some(more) if more > 0 => format!("{shown} and {more} more"),
        _ => shown,
    }
}

/// What the snapshot holds, when the trigger knows better than its tree id
/// (#549): a session's edits, or the index a commit records.
fn range_name(trigger: &Trigger, range: &str) -> Option<String> {
    let tree = range.split_once("..")?.1;
    let tree = &tree[..tree.len().min(10)];
    Some(match trigger {
        Trigger::Inspect {
            staged,
            own: Some(own),
        } if !own.others.is_empty() => format!(
            "this session's {}edits (snapshot tree {tree})",
            if *staged { "staged " } else { "" }
        ),
        Trigger::Inspect { staged: true, .. } | Trigger::Commit { amend: false, .. } => {
            format!("staged changes (index tree {tree})")
        }
        Trigger::Commit { amend: true, .. } => {
            format!("staged changes on HEAD~1 (index tree {tree})")
        }
        _ => return None,
    })
}

/// The definitions this session's edits change that a file read as
/// committed names in its pending text: no caller count of the answer
/// covers that text, so for them a zero is no finding (#453).
#[allow(
    clippy::disallowed_methods,
    reason = "the pending text of files the review reads as committed; one that cannot be read is named in the hedge"
)]
fn named_in_unread(root: &Path, own: &Owned) -> Option<String> {
    let Ok(names) = edited_definitions(root, Some(&own.files)) else {
        return Some(
            "(callers may be missing: the names this session's edits change could not be read, so the changed files read as committed were not checked for them)"
                .to_string(),
        );
    };
    let mut named = std::collections::BTreeSet::new();
    let mut naming = Vec::new();
    let mut unread = Vec::new();
    for path in &own.others {
        let text = match std::fs::read_to_string(root.join(path)) {
            Ok(text) => text,
            // A file the change deleted names nothing.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                unread.push(path.clone());
                continue;
            }
        };
        let words: rustc_hash::FxHashSet<&str> = text
            .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
            .collect();
        let hits: Vec<&String> = names
            .iter()
            .filter(|n| words.contains(n.as_str()))
            .collect();
        if !hits.is_empty() {
            named.extend(hits);
            naming.push(path.clone());
        }
    }
    let mut parts = Vec::new();
    if !named.is_empty() {
        parts.push(format!(
            "{} named in {} changed file(s) read as committed: {}",
            named.into_iter().cloned().collect::<Vec<_>>().join(", "),
            naming.len(),
            some_of(&naming)
        ));
    }
    if !unread.is_empty() {
        parts.push(format!(
            "{} changed file(s) read as committed could not be read: {}",
            unread.len(),
            some_of(&unread)
        ));
    }
    (!parts.is_empty()).then(|| format!("(callers may be missing: {})", parts.join("; ")))
}

/// A review the time ran out on says so and names what it may have left:
/// the range's files with no caller listed, which is either no caller found
/// or the run stopping before it reached them. `paths` are the range's,
/// read before the review began, `None` when git refused that listing.
fn stopped_note(
    output: &crate::impact::ImpactOutput,
    range: &str,
    paths: Option<&[String]>,
) -> String {
    if !output
        .limits
        .contains(&crate::resource::LimitReason::Deadline)
    {
        // A read the engine needed and did not get leaves the callers it
        // lists a lower bound: the answer above says "not determined" for
        // what it never searched, and this says why in a line of its own.
        return if output
            .limits
            .contains(&crate::resource::LimitReason::ReadFailed)
        {
            format!(
                "diffctx could not read part of {range} (read_failed): the callers listed above are a lower bound. `diffctx <repo> --diff {range} --mode impact` repeats the review.\n"
            )
        } else {
            String::new()
        };
    }
    // A file whose definitions the run never finished is listed above only as
    // "not determined": it still lists no caller.
    let listed: rustc_hash::FxHashSet<&str> = output
        .changed
        .iter()
        .filter(|c| !crate::impact::is_unfinished(c))
        .map(|c| c.path.as_str())
        .collect();
    let Some(paths) = paths else {
        return format!(
            "diffctx stopped at the hook's budget before it finished {range}. Its changed files could not be listed, so which of them the run left unreached is unknown. `diffctx <repo> --diff {range} --mode impact` runs the review to the end.\n"
        );
    };
    let unlisted: Vec<String> = paths
        .iter()
        .filter(|p| !listed.contains(p.as_str()))
        .cloned()
        .collect();
    let which = if unlisted.is_empty() {
        String::new()
    } else {
        format!(
            " {} of the {} changed file(s) list no caller above, because none was found or the run stopped before it reached them ({}).",
            unlisted.len(),
            paths.len(),
            some_of(&unlisted)
        )
    };
    format!(
        "diffctx stopped at the hook's budget before it finished {range}.{which} `diffctx <repo> --diff {range} --mode impact` runs the review to the end.\n"
    )
}

fn impact_context(
    event: Event,
    trigger: &Trigger,
    root: &Path,
    range: &str,
    spec: Option<snap::RangeSpec>,
    secs: u64,
    paths: Option<&[String]>,
) -> Result<Reply, Silence> {
    let mut output = run_impact(root, range, spec, secs)?;
    let hedge = match trigger {
        Trigger::Inspect { own: Some(own), .. } if !own.others.is_empty() => {
            named_in_unread(root, own)
        }
        _ => None,
    };
    if output.empty && hedge.is_none() {
        return Err("empty");
    }
    // The files it read reach nothing, and files it withheld could: said in one
    // line, never as "reaches nothing".
    if let Some(line) = crate::impact::unread_line(&output, range).filter(|_| hedge.is_none()) {
        return Ok(Reply {
            context: line.clone(),
            headline: line,
            partial: false,
        });
    }
    output.range_name = range_name(trigger, range);
    let partial = !output.limits.is_empty();
    let check = what_to_check(&output, hedge.is_some());
    let stopped = stopped_note(&output, range, paths);
    let text = crate::impact::render_automatic(&mut output, AUTOMATIC_TOKEN_CAP);
    let headline = text.lines().next().unwrap_or("").to_string();
    Ok(Reply {
        context: format!(
            "{} What it reaches outside the diff:\n{text}\n{stopped}{}{}",
            lead_of(event, trigger, range),
            hedge.map_or_else(String::new, |h| format!("{h}\n")),
            check.map_or_else(String::new, |c| format!(
                "Check {c} before going on, or tell the user why they are unaffected."
            )),
        ),
        headline,
        partial,
    })
}

/// What in a review a reader must act on, named; `None` when every row is
/// context — a caller a test reaches, a config naming the file — and a
/// constant "check these" under it taught skipping the block (#656).
fn what_to_check(output: &crate::impact::ImpactOutput, hedged: bool) -> Option<String> {
    let untested = crate::answer::Tally::of(&crate::impact::v2_of(output)).untested_callers;
    let mut parts: Vec<String> = Vec::new();
    if untested > 0 {
        parts.push(format!(
            "the {untested} caller(s) no static test link reaches"
        ));
    }
    if !output.stale_references.is_empty() || output.stale_references_omitted > 0 {
        parts.push("the lines still naming a removed path".to_string());
    }
    if !output.contracts.is_empty() || output.contracts_omitted > 0 {
        parts.push("the changed contract(s)".to_string());
    }
    if hedged && parts.is_empty() {
        parts.push("the files read as committed".to_string());
    }
    (!parts.is_empty()).then(|| parts.join(" and "))
}

/// A text search the agent ran: a Bash grep/rg/ag/`git grep`, or the Grep
/// tool.
struct Searched {
    dir: Option<String>,
    /// The paths the search reads; none when it reads the directory it ran in.
    paths: Vec<String>,
    pattern: String,
    inverted: bool,
    revisions: Vec<String>,
    /// The files its result names, when it names any: the search's own
    /// record of where the pattern is. `Some(empty)` when it found nothing.
    found: Option<Vec<String>>,
}

fn searched(payload: &serde_json::Value) -> Option<Searched> {
    let response = &payload["tool_response"];
    if payload["tool_name"] == "Grep" {
        let dir = payload["tool_input"]["path"].as_str().map(str::to_string);
        let found = response["filenames"].as_array().map(|files| {
            files
                .iter()
                .filter_map(|f| f.as_str().map(str::to_string))
                .collect()
        });
        return Some(Searched {
            paths: dir.iter().cloned().collect(),
            dir,
            pattern: payload["tool_input"]["pattern"].as_str()?.to_string(),
            inverted: false,
            revisions: Vec::new(),
            found,
        });
    }
    let command = payload["tool_input"]["command"].as_str()?;
    let Search {
        ran_in,
        dir,
        paths,
        pattern,
        inverted,
        revisions,
    } = intent::search(command)?;
    let found = response["stdout"].as_str().and_then(|out| {
        let cwd = payload_cwd(payload)?;
        let at = resolve_dir(&cwd, ran_in.as_deref());
        files_named_in(out, &at)
    });
    Some(Searched {
        dir,
        paths,
        pattern,
        inverted,
        revisions,
        found,
    })
}

/// The files a search's output names at the head of its lines
/// (`path:line:text`, `path` alone for `-l`), as paths under `at`. `None`
/// when no line names one: a single file's matches, a count.
fn files_named_in(output: &str, at: &Path) -> Option<Vec<String>> {
    if output.trim().is_empty() {
        return Some(Vec::new());
    }
    let mut files: Vec<String> = output
        .lines()
        .take(2000)
        .filter_map(|line| {
            let head = line.split(':').next()?.trim();
            let path = at.join(head.strip_prefix("./").unwrap_or(head));
            (!head.is_empty() && path.is_file()).then(|| path.to_string_lossy().into_owned())
        })
        .collect();
    files.sort();
    files.dedup();
    (!files.is_empty()).then_some(files)
}

/// Where a search looked, as repo-relative prefixes: the files its result
/// names, else its paths, else the directory it ran in (#646, fj#95). A
/// path outside the repository adds none; an empty prefix is the whole
/// repository.
struct Scope(Vec<String>);

impl Scope {
    /// Every place the search looked is a file of prose (`.md`, `.txt`): decided
    /// from the names alone, before any symbol is built.
    fn reads_only_prose(&self, root: &Path) -> bool {
        !self.0.is_empty()
            && self.0.iter().all(|p| {
                let path = root.join(p);
                let ext = path.extension().map_or_else(String::new, |e| {
                    format!(".{}", e.to_string_lossy().to_lowercase())
                });
                !p.is_empty()
                    && path.is_file()
                    && crate::config::extensions::DOC_EXTENSIONS.contains(ext.as_str())
            })
    }

    fn holds(&self, path: &str) -> bool {
        self.0.iter().any(|p| {
            p.is_empty()
                || path == p
                || path
                    .strip_prefix(p.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        })
    }
}

fn search_scope(payload: &serde_json::Value, search: &Searched, root: &Path) -> Scope {
    let cwd = payload_cwd(payload);
    let looked: Vec<Option<&str>> = match (&search.found, search.paths.is_empty()) {
        (Some(found), _) if !found.is_empty() => found.iter().map(|p| Some(p.as_str())).collect(),
        (_, true) => vec![None],
        (_, false) => search.paths.iter().map(|p| Some(p.as_str())).collect(),
    };
    let root = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let prefixes: Vec<String> = cwd
        .iter()
        .flat_map(|cwd| looked.iter().map(move |p| resolve_dir(cwd, *p)))
        .filter_map(|p| {
            let p = before_glob(&p);
            let p = dunce::canonicalize(&p).unwrap_or(p);
            // A path outside the repository adds no prefix.
            let Ok(rel) = p.strip_prefix(&root) else {
                return None;
            };
            Some(rel.to_string_lossy().replace('\\', "/"))
        })
        .collect();
    Scope(prefixes)
}

/// The names of the definitions the pending change edits, in `within` when
/// it names files: the innermost named container around each hunk of the
/// working tree, the way impact names its changed symbols. Only the changed
/// files are parsed, so a search for any other name stops here without
/// running the pipeline.
fn edited_definitions(root: &Path, within: Option<&[String]>) -> Result<Vec<String>, Silence> {
    // The pending change as impact reads it, untracked files included: a
    // name defined only in a new file is still the change's (#448).
    let run = crate::resource::RunContext::unbounded();
    let analysis = crate::source::Analysis::at_working_tree(root, &run).map_err(|_| "error:git")?;
    run.set_source(analysis.head());
    let (mut hunks, _) = crate::pipeline::collect_hunks_with_untracked(
        root,
        Some("HEAD"),
        &[],
        true,
        &run.source(),
        None,
    )
    .map_err(|_| "error:git")?;
    if let Some(files) = within {
        let top = canonical(root);
        let files: rustc_hash::FxHashSet<PathBuf> = files.iter().map(|f| top.join(f)).collect();
        hunks.retain(|h| files.contains(&canonical(Path::new(&*h.path))));
    }
    let mut files: Vec<PathBuf> = hunks.iter().map(|h| PathBuf::from(&*h.path)).collect();
    files.sort();
    files.dedup();
    let fragments = crate::fragmentation::fragment_files(
        &files,
        root,
        &mut rustc_hash::FxHashSet::default(),
        true,
        &run,
        &hunks,
    );
    let mut names = Vec::new();
    for hunk in &hunks {
        let around: Vec<&crate::types::Fragment> = fragments
            .iter()
            .filter(|f| {
                f.path() == &*hunk.path
                    && f.kind.is_definition_kind()
                    && f.symbol_name.is_some()
                    && f.id.start_line <= hunk.end_line()
                    && f.id.end_line >= hunk.new_start
            })
            .collect();
        let innermost = around.iter().filter(|outer| {
            !around.iter().any(|inner| {
                inner.id != outer.id
                    && inner.id.start_line >= outer.id.start_line
                    && inner.id.end_line <= outer.id.end_line
            })
        });
        names.extend(innermost.filter_map(|f| f.symbol_name.clone()));
    }
    names.sort();
    names.dedup();
    Ok(names)
}

/// The pending change's callers by symbol name, in the impact's own
/// rendering. `partial` is a run that stopped at a limit: a name missing
/// from it is no proof that nothing calls it.
struct Known {
    answers: std::collections::BTreeMap<String, String>,
    partial: bool,
    /// Names edited only in comments, formatting or position: their callers
    /// were not traced, which is no finding that there are none (#498).
    layout: std::collections::BTreeSet<String>,
}

fn known_answers(mut output: crate::impact::ImpactOutput) -> Known {
    let layout = crate::impact::layout_only_names(&output)
        .map(str::to_string)
        .collect();
    let mut by_name: std::collections::BTreeMap<String, Vec<crate::impact::ChangedSymbol>> =
        std::collections::BTreeMap::new();
    for sym in std::mem::take(&mut output.changed) {
        if let Some(name) = sym.symbol.clone() {
            by_name.entry(name).or_default().push(sym);
        }
    }
    output.contracts.clear();
    output.contracts_omitted = 0;
    output.stale_references.clear();
    output.stale_references_omitted = 0;
    output.empty = false;
    let partial = !output.limits.is_empty();
    // A symbol listed with no callers but an `unresolved` note has an
    // answer too: that its callers could not be resolved.
    let answers = by_name
        .into_iter()
        .filter(|(_, syms)| {
            syms.iter()
                .any(|s| !s.callers.is_empty() || s.unresolved.is_some() || !s.missed_in.is_empty())
        })
        .map(|(name, syms)| {
            let mut one = crate::impact::ImpactOutput {
                changed: syms,
                ..output.clone()
            };
            let text = crate::impact::render_automatic(&mut one, AUTOMATIC_TOKEN_CAP);
            (name, text)
        })
        .collect();
    Known {
        answers,
        partial,
        layout,
    }
}

/// The names among `names` that a file the search looked in defines or
/// references, when every place it looked is a file whose language the facts
/// layer reads. A name its matched lines spell only in a comment or a string
/// has no definition and no caller there: the answer ends `out-of-scope`, and
/// building it to find out cost seconds a search (#667). `None` when a place
/// is a directory, the whole repository, a file of a language the facts do not
/// read or one that did not parse whole: the lookup decides.
#[allow(
    clippy::disallowed_methods,
    reason = "a pre-filter: None sends every name to the full lookup, which reads through the snapshot"
)]
fn named_in_scope(
    root: &Path,
    scope: &Scope,
    names: &[String],
) -> Option<rustc_hash::FxHashSet<String>> {
    use crate::facts::Fact;
    const MAX_READ: u64 = 1024 * 1024;
    // Past a few files the reading costs what the lookup would.
    const MAX_PLACES: usize = 20;
    if scope.0.is_empty() || scope.0.len() > MAX_PLACES {
        return None;
    }
    let registry = crate::facts::lang::builtin();
    let mut spelled: rustc_hash::FxHashSet<String> = rustc_hash::FxHashSet::default();
    for place in &scope.0 {
        let path = root.join(place);
        if place.is_empty() || !path.is_file() || std::fs::metadata(&path).ok()?.len() > MAX_READ {
            return None;
        }
        let language = crate::facts::Language::from_path(&snap::RepoPath::new(place).ok()?)?;
        let bytes = std::fs::read(&path).ok()?;
        let blob = registry
            .get(language)?
            .extract(&bytes, &crate::snapshot::RunBudget::default());
        if !blob.losses.is_empty() {
            return None;
        }
        for fact in &blob.facts {
            let name = match fact {
                Fact::Definition(def) => def.local.name.as_str(),
                Fact::Reference(reference) => reference.name.as_str(),
                _ => continue,
            };
            if names.iter().any(|n| n == name) {
                spelled.insert(name.to_string());
            }
        }
    }
    Some(spelled)
}

/// A search answers every name it is a search for (#634): a name the
/// pending change edits from the impact, which already has its callers
/// (#337), any other as `--symbol` would. Silence when none has an answer.
fn search_answer(
    payload: &serde_json::Value,
    search: &Searched,
    started: Instant,
    trace: &mut Trace,
) -> Result<String, Silence> {
    // A search for what does not spell the name, or that found nothing, is
    // no lookup of a symbol.
    if search.inverted {
        return Err("not-a-lookup");
    }
    if search.found.as_ref().is_some_and(Vec::is_empty) {
        return Err("nothing-found");
    }
    let names = pattern_names(&search.pattern);
    if names.is_empty() {
        return Err("no-symbol");
    }
    let root = repo_root_checked(payload, search.dir.as_deref())?.ok_or("not-git")?;
    trace.root = Some(root.clone());
    trace.range = Some("HEAD".to_string());
    // A revision other than HEAD is a tree the answer would not be about: a
    // subagent's `git grep x origin/main` got 17 answers from a checkout 36
    // commits behind it.
    let head = snap::rev_oid(&root, "HEAD").map_err(|_| "error:git")?;
    for revision in &search.revisions {
        if snap::rev_oid(&root, revision).map_err(|_| "error:git")? != head {
            return Err("other-tree");
        }
    }
    let scope = search_scope(payload, search, &root);
    // A pending change git would not read leaves unknown which names it
    // edits: said, never answered as if nothing were edited.
    let (identity, mut unread) = match result_identity(&root, "HEAD") {
        Ok(identity) => (identity, false),
        Err(_) => (None, true),
    };
    let edited = match &identity {
        Some(_) => edited_definitions(&root, None).unwrap_or_else(|_| {
            unread = true;
            Vec::new()
        }),
        None => Vec::new(),
    };
    let (edited, others): (Vec<String>, Vec<String>) =
        names.into_iter().partition(|n| edited.contains(n));
    let mut answers = Vec::new();
    if unread {
        answers.push(
            "diffctx could not read the pending change (git refused a read): which of the searched names it edits is unknown, and the answers here are of HEAD as committed."
                .to_string(),
        );
    }
    let mut silence = "seen";
    // A name no searched file defines or calls is no lookup worth a build.
    let spelled = named_in_scope(&root, &scope, &others);
    let others: Vec<String> = match &spelled {
        Some(spelled) => others.into_iter().filter(|n| spelled.contains(n)).collect(),
        None => others,
    };
    if spelled.is_some() && others.is_empty() {
        silence = "out-of-scope";
    }
    if let (Some(identity), false) = (&identity, edited.is_empty()) {
        match edited_answer(&root, identity, &edited, started, trace) {
            Ok(text) => answers.push(text),
            Err(why) => silence = why,
        }
    }
    if !others.is_empty() && scope.reads_only_prose(&root) {
        // Prose holds no definition and no resolved caller: nothing to
        // build an answer from (gh#667).
        silence = "out-of-scope";
    } else if !others.is_empty() {
        let lookup = is_lookup_pattern(&search.pattern);
        match symbol_answer(&root, &others, &scope, lookup, started, trace) {
            Ok(text) => answers.push(text),
            Err(why) => silence = why,
        }
    }
    if answers.is_empty() {
        return Err(silence);
    }
    Ok(answers.join("\n"))
}

/// Names the pending change edits, answered from its impact once per name
/// per identity per session, marked only once answered.
fn edited_answer(
    root: &Path,
    identity: &str,
    names: &[String],
    started: Instant,
    trace: &mut Trace,
) -> Result<String, Silence> {
    let session = trace.session.clone();
    let key = |name: &str| session_key(&format!("id-{identity}.{name}"), session.as_deref());
    let names: Vec<&String> = names
        .iter()
        .filter(|n| delivered(&key(n), SEEN_TTL).is_none())
        .collect();
    if names.is_empty() {
        return Err("seen");
    }
    let tries = session_key(&format!("try-{identity}.search"), session.as_deref());
    let attempt = attempts(&tries);
    if attempt >= MAX_ATTEMPTS {
        return Err("retry-exhausted");
    }
    mark_seen_with(&tries, &(attempt + 1).to_string());
    let root = root.to_path_buf();
    let known = match within_budget(started, move |secs| {
        run_impact(&root, "HEAD", None, secs).map(known_answers)
    }) {
        // Said, never silence: the name was searched, and the reader cannot
        // tell a quiet analysis from one that did not finish. It stays
        // unmarked (and counted as an attempt), so a later search tries again.
        Err("timeout") => {
            trace.noted = Some("timeout");
            let first = names[0];
            return Ok(format!(
                "diffctx: the pending change edits {}; the analysis of what references {} ran past the hook's deadline and was not finished. `diffctx . --symbol {first} -f md` runs it without one.",
                quoted(&names),
                if names.len() == 1 { "it" } else { "them" },
            ));
        }
        known => known?,
    };
    unmark(&tries);
    let mut answers = Vec::new();
    for name in names {
        let answer = match known.answers.get(name) {
            Some(text) => format!(
                "You searched for `{name}`, which the pending change edits; diffctx already analysed what references it:\n{text}"
            ),
            None if known.layout.contains(name) => format!(
                "diffctx: `{name}` is edited by the pending change only in comments, formatting or position, which no caller sees, so its callers were not traced; `diffctx . --symbol {name}` lists them."
            ),
            None if !known.partial => format!(
                "diffctx: `{name}` is edited by the pending change, and no resolved static caller outside the diff was found in the analysed scope."
            ),
            None => continue,
        };
        mark_delivered(&key(name), &answer, known.partial);
        answers.push(answer);
    }
    if answers.is_empty() {
        return Err("partial");
    }
    Ok(answers.join("\n"))
}

/// The automatic answer to a search for a name, a pointer to the full one: the
/// whole text the agent reads for one name, lead line included (AC8).
const SYMBOL_TOKEN_CAP: u32 = 150;

/// More definitions than this make a searched name ambiguous.
const MAX_SEARCH_DEFINITIONS: usize = 3;

/// Names one search answers as `--symbol` would. Each runs the pipeline
/// within the hook's one deadline, so a name past it is said and left for a
/// later search: about one answered search in ten seeks a second name.
const MAX_SEARCH_NAMES: usize = 3;

/// A search for a name the pending change does not edit, on any tree: where
/// it is defined, who calls it and which tests reach them, as `--symbol`
/// answers it, once per name and commit in a session. Most lookups come on
/// a clean tree, where the hook used to say nothing (#628).
fn symbol_answer(
    root: &Path,
    names: &[String],
    scope: &Scope,
    lookup: bool,
    started: Instant,
    trace: &mut Trace,
) -> Result<String, Silence> {
    // An unborn branch has no head: its key is its own.
    let head = snap::rev_oid(root, "HEAD")
        .map_err(|_| "error:git")?
        .unwrap_or_else(|| "unborn".to_string());
    let session = trace.session.clone();
    let key = |name: &str| session_key(&format!("sym-{head}-{name}"), session.as_deref());
    let fresh: Vec<&String> = names
        .iter()
        .filter(|n| !is_seen(&key(n)))
        .take(MAX_SEARCH_NAMES)
        .collect();
    if fresh.is_empty() {
        return Err("seen");
    }
    let mut answers = Vec::new();
    let mut silence = "empty";
    let mut cut: Vec<&String> = Vec::new();
    for name in fresh {
        mark_seen(&key(name));
        match one_symbol_answer(root, name, scope, lookup, started) {
            Ok(text) => answers.push(text),
            // The budget is spent: the name is said, and left for a later
            // search; the names after it were never started.
            Err("timeout") => {
                unmark(&key(name));
                cut.push(name);
                break;
            }
            Err(why) => {
                // A shape that is no lookup does not use the name up.
                if why == "not-a-lookup" {
                    unmark(&key(name));
                }
                silence = why;
            }
        }
    }
    if let Some(first) = cut.first() {
        trace.noted = Some("timeout");
        answers.push(format!(
            "diffctx: the lookup of {} ran past the hook's deadline and was not finished; `diffctx . --symbol {first} -f md` runs it without one.",
            quoted(&cut),
        ));
    }
    if answers.is_empty() {
        return Err(silence);
    }
    Ok(answers.join("\n"))
}

/// Names, each in code quotes, for a sentence.
fn quoted(names: &[&String]) -> String {
    names
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn one_symbol_answer(
    root: &Path,
    name: &str,
    scope: &Scope,
    is_lookup: bool,
    started: Instant,
) -> Result<String, Silence> {
    let (root, lookup) = (root.to_path_buf(), name.to_string());
    let mut output = within_budget(started, move |secs| {
        let scoring = ScoringMode::from_str(DEFAULT_SCORING).map_err(|_| "error:scoring")?;
        crate::pipeline::build_symbol_impact(&root, &lookup, &[], DEFAULT_PPR_ALPHA, scoring, secs)
            .map_err(|_| "no-symbol")
    })?;
    // No definition, but files spell it: one line, since the grep that led
    // here printed the lines and cannot say no binding was found (R2). Only a
    // lookup of a name code would spell asks that: an alternation or a plain
    // word is a search for text.
    if let Some(named) = &output.symbol_named {
        if !is_lookup {
            return Err("not-a-lookup");
        }
        if !crate::impact::distinctive(name) {
            return Err("no-symbol");
        }
        return Ok(format!(
            "`{name}`: no definition; named textually in {} file(s), no binding \u{2014} `diffctx . --symbol {name} -f md` lists them",
            named.files
        ));
    }
    // A definition outside where the search looked, with no caller there,
    // is another symbol of the same name: a `models` module of another
    // subproject, a `segments` the grepped file never reaches (#646, fj#95).
    let defined = output.changed.len();
    output
        .changed
        .retain(|c| scope.holds(&c.path) || c.callers.iter().any(|k| scope.holds(&k.path)));
    if output.changed.is_empty() {
        return Err("out-of-scope");
    }
    // The answer adds what grep cannot: the callers it resolved and the
    // tests that reach them. Without one it says nothing, because a caller
    // in a file no model reads (an `.astro` page, a template, a class name
    // in markup) would read as none. A word match is no resolved caller:
    // the grep that led here printed those lines already.
    if output
        .changed
        .iter()
        .all(|c| c.callers.iter().all(crate::impact::is_textual))
    {
        return Err("empty");
    }
    // A name defined all over (`main`, `run`, `new`) is no lookup a short
    // answer can serve: its first definitions crowd out the one searched.
    if output.definitions() > MAX_SEARCH_DEFINITIONS {
        return Err("ambiguous");
    }
    let elsewhere = match defined - output.changed.len() {
        0 => String::new(),
        n => format!(" where you searched ({n} more definition(s) elsewhere)"),
    };
    let lead = format!(
        "You searched for `{name}`; diffctx resolved it{elsewhere} (`diffctx . --symbol {name} -f md` gives the whole answer):\n"
    );
    let room = SYMBOL_TOKEN_CAP.saturating_sub(crate::tokenizer::count_tokens(&lead));
    let text = crate::impact::render_automatic(&mut output, room);
    Ok(format!("{lead}{text}"))
}

/// The JSON the hook prints. The strict gate denies the command with the
/// impact attached: the content is now marked reviewed, so the same command
/// passes on the retry.
fn hook_json(event: Event, context: &str, gate: bool) -> String {
    let mut out = serde_json::json!({
        "hookEventName": event.name(),
        "additionalContext": context,
    });
    if gate && event == Event::PreToolUse {
        out["permissionDecision"] = serde_json::Value::String("deny".to_string());
        // The reason is the one field a denied call is guaranteed to show the
        // model, so the impact rides in it, not only in additionalContext.
        out["permissionDecisionReason"] = serde_json::Value::String(format!(
            "diffctx impact gate: this change was not reviewed yet. {context}\nRun the same command again to proceed."
        ));
    }
    serde_json::json!({ "hookSpecificOutput": out }).to_string()
}

/// `work` inside the hook's one budget (Forgejo #113): the review of a range,
/// the analysis of a pending change, the lookup of a name. The engine's own
/// deadline is cooperative: it stops the phase it is in, then renders what it
/// read, and that tail has no deadline of its own. The engine therefore gets
/// its share of what is left of the ceiling, not of the whole, so time the
/// hook spent before it (or on the name before) no longer comes out of the
/// tail; and at the ceiling the hook waits `TAIL_GRACE_SECS` more for the
/// partial answer the engine is already rendering, where it used to drop it
/// for a bare timeout. Only a run that does not return in that time is a
/// timeout, and every caller says so by name.
fn within_budget<T: Send + 'static>(
    started: Instant,
    work: impl FnOnce(u64) -> Result<T, Silence> + Send + 'static,
) -> Result<T, Silence> {
    let left = HOOK_DEADLINE_SECS.saturating_sub(started.elapsed().as_secs());
    if left == 0
        || std::env::var(crate::resource::TEST_HOOK_CEILING_EXPIRED_ENV).as_deref() == Ok("1")
    {
        return Err("timeout");
    }
    let engine_secs = (left * PIPELINE_SECS / HOOK_DEADLINE_SECS).max(1);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(crate::profile::carry(move || {
        let _ = tx.send(work(engine_secs));
    }));
    for wait in [left, TAIL_GRACE_SECS] {
        match rx.recv_timeout(Duration::from_secs(wait)) {
            Ok(result) => return result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Err("error:panic"),
        }
    }
    Err("timeout")
}

/// The files a range changes, read before its review begins: past the
/// ceiling the hook's own git calls are refused, and a review cut short must
/// still name what it may have left. `None` when git refused the listing:
/// the files are unknown, which is not an empty list.
#[allow(
    clippy::disallowed_methods,
    reason = "None is a listing git refused, which the answer says: \"Its changed files could not be listed.\""
)]
fn range_paths(root: &Path, range: &str) -> Option<Vec<String>> {
    let args = ["diff", "--name-only", "-z", "--no-renames", range, "--"];
    snap::run_git_z(root, &args).ok()
}

/// The hook's whole decision on one stdin payload. An answer is marked
/// delivered only once it is returned for printing; a run that timed out or
/// failed is retried on a later command, at most `MAX_ATTEMPTS` times per
/// identity, and never counts as a review.
fn decide(
    event: Event,
    stdin_json: &str,
    gate: bool,
    started: Instant,
    trace: &mut Trace,
) -> Result<String, Silence> {
    let answer = review(event, stdin_json, gate, started, trace);
    if !matches!(event, Event::PostToolUse { .. }) {
        return answer;
    }
    // A payload that does not parse is `review`'s `error:payload` already.
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(stdin_json) else {
        return answer;
    };
    let Some(note) = names_other_repositories(&payload) else {
        return answer;
    };
    match answer {
        Ok(text) => Ok(with_notice(&text, &note)),
        Err(_) => {
            trace.noted = Some("elsewhere");
            Ok(hook_json(event, &note, false))
        }
    }
}

/// The line points git at a repository other than the call's own and its
/// work trees (`cd`, `-C`, `--git-dir`, a variable): git's record of those
/// was not read, and the line's text is not evidence of what it did there.
/// Said, so that silence does not read as a clean review.
fn names_other_repositories(payload: &serde_json::Value) -> Option<String> {
    let command = payload["tool_input"]["command"].as_str()?;
    let places = intent::places(command);
    if places.is_empty() || searched(payload).is_some() {
        return None;
    }
    let cwd = payload_cwd(payload)?;
    let mut own: Vec<PathBuf> = Vec::new();
    if let Ok(home) = snap::open(&cwd) {
        own.push(canonical(&home.work_tree));
        if let Ok(trees) = work_trees(&home) {
            own.extend(trees.into_iter().map(|(work_tree, _)| work_tree));
        }
    }
    let elsewhere = places.iter().any(|place| match place {
        intent::Place::Unknown => true,
        intent::Place::Dir(dir) => {
            let dir = resolve_dir(&cwd, Some(dir));
            // A directory that is gone (a work tree the line removed) holds
            // nothing to have missed.
            dir.exists()
                && match snap::find_toplevel(&dir) {
                    Ok(Some(top)) => !own.contains(&canonical(&top)),
                    // No work tree, or git could not say: the notice that the
                    // call's own repository was the only one read stands.
                    Ok(None) | Err(_) => true,
                }
        }
    });
    elsewhere.then(|| {
        "diffctx did not analyse this command in the repository it names with `cd`, `-C` or `--git-dir`: git's record was read only for the repository the call started in and its work trees. Run `diffctx <repo> --diff --mode impact` in the other repository before going on."
            .to_string()
    })
}

fn review(
    event: Event,
    stdin_json: &str,
    gate: bool,
    started: Instant,
    trace: &mut Trace,
) -> Result<String, Silence> {
    let payload: serde_json::Value =
        serde_json::from_str(stdin_json).map_err(|_| "error:payload")?;
    trace.session = payload["session_id"].as_str().map(str::to_string);
    trace.call = payload["tool_use_id"]
        .as_str()
        .map(|id| {
            id.chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                .take(64)
                .collect::<String>()
        })
        .filter(|id| !id.is_empty());
    if matches!(event, Event::PostToolUse { .. }) {
        bracket_after(&payload);
    }
    let command = payload["tool_input"]["command"].as_str().unwrap_or("");
    let recording = !command.is_empty();
    let search = if matches!(event, Event::PostToolUse { .. }) {
        searched(&payload)
    } else {
        None
    };
    // What a line recorded outranks a search on the same line: `git commit
    // … ; grep … log` is a commit to review, not a lookup (#527).
    let answer_search = |search: &Searched, trace: &mut Trace| {
        trace.verb = Some("grep");
        search_answer(&payload, search, started, trace).map(|c| hook_json(event, &c, false))
    };
    if let (Some(search), false) = (&search, recording) {
        return answer_search(search, trace);
    }
    if event == Event::PreToolUse && recording {
        let recorded = record_before(&payload, command, trace);
        let intent = intent::of(false, command);
        let gated_commit = gate && matches!(intent, Some(Intent::Commit { .. }));
        let opens_pr = matches!(intent, Some(Intent::PullRequest { .. }));
        match recorded {
            Ok(()) if !gated_commit && !opens_pr => {
                return Err("recorded");
            }
            Ok(()) | Err("not-git") => {}
            Err(why) => return Ok(not_analysed(event, why, trace)),
        }
    }
    if gate && event == Event::PreToolUse {
        if let Some(Intent::Commit { plain: false, .. }) = intent::of(false, command) {
            trace.noted = Some("denied");
            return Ok(serde_json::json!({ "hookSpecificOutput": {
                "hookEventName": event.name(),
                "permissionDecision": "deny",
                "permissionDecisionReason": "diffctx's strict gate reviews the index a commit records, so it does not run a commit that stages for itself. Run the staging (`git add`, `git rm`, ...) as its own command, then `git commit` without paths, `-a`, `-i` or `--pathspec-from-file`.",
            }})
            .to_string());
        }
    }
    let prepared = if matches!(event, Event::PostToolUse { .. }) && recording {
        match prepare_landed(&payload, command, trace) {
            Err("no-record" | "error:record" | "nothing-landed") => match &search {
                Some(search) => return answer_search(search, trace),
                None => prepare(event, &payload, trace).map(|p| vec![p]),
            },
            Err(why @ "error:git") => return Ok(not_analysed(event, why, trace)),
            // What landed changes no file (an empty commit): said, so that
            // silence does not read as a clean review.
            Err("clean") => return Ok(nothing_changed(event, trace)),
            landed => landed,
        }
    } else {
        prepare(event, &payload, trace).map(|p| vec![p])
    };
    let prepared = match prepared {
        Ok(prepared) => prepared,
        // The inspected files may be the edits of a Bash call the hook could
        // not bracket: said, never guessed from a file's time.
        Err("unbracketed") => return Ok(not_attributed(event, trace)),
        // A read git refused is no clean change: said, with what is known.
        Err(why @ "error:git") => {
            let range = trace
                .range
                .clone()
                .unwrap_or_else(|| "the pending change".into());
            return Ok(not_reviewed(event, &range, why, Some(&[]), trace));
        }
        Err(why) => return Err(why),
    };
    let mut answers = Vec::new();
    let mut silence = None;
    for p in prepared {
        match answer(event, p, gate, started, trace) {
            Ok(a) => answers.push(a),
            Err(why) => silence = silence.or(Some(why)),
        }
    }
    joined(answers).ok_or(silence.unwrap_or("empty"))
}

/// The marker that this session's own hook reviewed `oid` when it landed.
fn reviewed_key(oid: &str, session: Option<&str>) -> String {
    session_key(&format!("rev-{oid}"), session)
}

/// The commits of `range`, newest first, up to the cap a review lists.
fn commits_of(root: &Path, range: &str) -> Result<Vec<String>, Silence> {
    Ok(git_checked(root, &["rev-list", "-n", "500", range, "--"])?
        .map_or_else(Vec::new, |list| list.lines().map(str::to_string).collect()))
}

/// A review of what a call committed is complete: each commit of its range is
/// marked, so a later push of them is not reviewed a second time (#640).
fn note_reviewed(landed: &Option<(PathBuf, String)>, trace: &Trace) {
    let (Some((root, range)), false) = (landed, file_safe(trace.session.as_deref()).is_empty())
    else {
        return;
    };
    // Commits git cannot list stay unmarked: a later push of them is reviewed
    // again, which costs a repeat and hides nothing.
    let Ok(commits) = commits_of(root, range) else {
        return;
    };
    for oid in commits {
        mark_seen(&reviewed_key(&oid, trace.session.as_deref()));
    }
}

/// A push whose every commit this session's earlier reviews covered: what it
/// publishes was shown when it landed.
fn pushes_only_reviewed(root: &Path, range: &str, session: Option<&str>) -> bool {
    if file_safe(session).is_empty() {
        return false;
    }
    // A push git cannot list is reviewed in full.
    let Ok(commits) = commits_of(root, range) else {
        return false;
    };
    !commits.is_empty()
        && commits.len() < 500
        && commits
            .iter()
            .all(|oid| is_seen(&reviewed_key(oid, session)))
}

/// The answer for one prepared range: a reminder of what was delivered
/// before, the review, or why there is none.
fn answer(
    event: Event,
    prepared: Prepared,
    gate: bool,
    started: Instant,
    trace: &mut Trace,
) -> Result<String, Silence> {
    if event == Event::PreToolUse && is_seen(&prepared.record_key) {
        return Err("seen");
    }
    let landed = matches!(prepared.trigger, Trigger::Landed { .. });
    // The commits a call made, whose complete review is marked below.
    let made = match (&prepared.trigger, &prepared.spec) {
        (Trigger::Landed { .. }, None) => Some((prepared.root.clone(), prepared.range.clone())),
        _ => None,
    };
    if let (
        Trigger::Landed {
            verb: "push",
            unmade,
            ..
        },
        Some(snap::RangeSpec::Pushed { .. }),
    ) = (&prepared.trigger, &prepared.spec)
    {
        if unmade.is_empty()
            && pushes_only_reviewed(&prepared.root, &prepared.range, trace.session.as_deref())
        {
            trace.noted = Some("reminder");
            return Ok(hook_json(
                event,
                &format!(
                    "diffctx: this push publishes ({}) only commits this session's calls made and had reviewed when they landed; none of it is new to review.",
                    prepared.range
                ),
                false,
            ));
        }
    }
    match (&prepared.shown, event) {
        // An answer that was empty stays empty: no reminder of callers that
        // were never listed. A landed record still says it in one line.
        (Some(d), _) if !d.partial && d.headline.is_empty() => {
            if event == Event::PreToolUse {
                mark_seen(&prepared.record_key);
            }
            note_reviewed(&made, trace);
            if landed {
                return Ok(nothing_reached(event, landed, trace));
            }
            return Err("empty");
        }
        // What landed was reviewed before (the gate, an inspection, a manual
        // run): pointed at, never silence, which read as no review (#501).
        (Some(d), _) if !d.partial && (event == Event::PreToolUse || landed) => {
            mark_seen(&prepared.record_key);
            note_reviewed(&made, trace);
            trace.noted = Some("reminder");
            return Ok(hook_json(event, &reminder(event, &d.headline), false));
        }
        (Some(d), Event::PostToolUse { .. }) if !d.partial => return Err("seen"),
        _ => {}
    }
    let attempt = attempts(&prepared.tries_key);
    if attempt >= MAX_ATTEMPTS {
        if landed {
            return Ok(not_reviewed(
                event,
                &prepared.range,
                "retry-exhausted",
                Some(&[]),
                trace,
            ));
        }
        return Err("retry-exhausted");
    }
    mark_seen_with(&prepared.tries_key, &(attempt + 1).to_string());
    let Prepared {
        root,
        range,
        spec,
        trigger,
        key,
        record_key,
        tries_key,
        unread,
        ..
    } = prepared;
    let reviewed = range.clone();
    let paths = range_paths(&root, &range);
    let named = paths.clone();
    let recorded = !matches!(trigger, Trigger::Inspect { .. });
    let answer = within_budget(started, move |secs| {
        impact_context(event, &trigger, &root, &range, spec, secs, named.as_deref())
    });
    let answer = with_unread(answer, &unread);
    match &answer {
        // Nothing to say is a complete answer, delivered as silence.
        Err("empty") => {
            mark_delivered(&key, "", false);
            note_reviewed(&made, trace);
        }
        Ok(a) => {
            mark_delivered(&key, &a.headline, a.partial);
            if !a.partial {
                note_reviewed(&made, trace);
            }
        }
        // A change that was not reviewed is said so, and which files it
        // changes: silence there reads as a clean review (#612, #613).
        // An inspection too says so when git or the engine failed: only a
        // limit it stopped at (a timeout) stays silent there.
        Err(why) if recorded || why.starts_with("error:") => {
            return Ok(not_reviewed(event, &reviewed, why, paths.as_deref(), trace));
        }
        Err(_) => return answer.map(|_| String::new()),
    }
    // A partial answer keeps its attempt: it is retried, but not forever.
    if !matches!(&answer, Ok(a) if a.partial) {
        unmark(&tries_key);
    }
    if event == Event::PreToolUse {
        mark_seen(&record_key);
    }
    match answer {
        Err("empty") if event == Event::PreToolUse || landed => {
            Ok(nothing_reached(event, landed, trace))
        }
        answer => answer.map(|a| {
            let deny = gate && !a.partial;
            if deny {
                trace.noted = Some("denied");
            } else if a.partial {
                // A review the run stopped short of is not a full one in the
                // log either (fj#127).
                trace.noted = Some("partial");
            }
            hook_json(event, &a.context, deny)
        }),
    }
}

/// A review of a pending change that left untracked directories unread says
/// so, naming them: what it found is a lower bound, and a change that sits
/// only in them is no clean one.
fn with_unread(answer: Result<Reply, Silence>, unread: &[String]) -> Result<Reply, Silence> {
    if unread.is_empty() {
        return answer;
    }
    let note = format!(
        "diffctx did not read {} untracked director(y/ies) of this checkout, whose files are not part of this review: {}.",
        unread.len(),
        some_of(unread)
    );
    match answer {
        Ok(mut reply) => {
            reply.context.push('\n');
            reply.context.push_str(&note);
            reply.partial = true;
            Ok(reply)
        }
        Err("empty") => Ok(Reply {
            context: note.clone(),
            headline: note,
            partial: true,
        }),
        other => other,
    }
}

/// A line that records or publishes commits: the events whose silence would
/// otherwise read as a clean review.
/// One line, so that no block means the hook did not run (#464).
fn nothing_reached(event: Event, landed: bool, trace: &mut Trace) -> String {
    trace.noted = Some("empty");
    let range = trace.range.as_deref().unwrap_or("this change");
    // What a call landed is what changed in the repository during it: git
    // cannot tell a commit another session made meanwhile from its own.
    let scope = if landed {
        ", covering what changed in this repository during this call"
    } else {
        ""
    };
    hook_json(
        event,
        &format!("diffctx: {range} reaches nothing outside the diff{scope}."),
        false,
    )
}

/// What the call recorded touches no file: one line, as `nothing_reached` is.
fn nothing_changed(event: Event, trace: &mut Trace) -> String {
    trace.noted = Some("empty");
    let range = trace.range.as_deref().unwrap_or("this change");
    hook_json(
        event,
        &format!(
            "diffctx: {range} changes no file, so nothing reaches outside it (it covers what changed in this repository during this call)."
        ),
        false,
    )
}

fn not_reviewed(
    event: Event,
    range: &str,
    why: Silence,
    paths: Option<&[String]>,
    trace: &mut Trace,
) -> String {
    trace.noted = Some(why);
    let how = match why {
        "timeout" => "the review ran past the hook's deadline",
        "retry-exhausted" => "earlier attempts at this change stopped at a limit",
        "error:deadline" => "the engine's deadline passed before it had an answer to give",
        "error:git" => "git refused a read the review needs (exit 3 on the command line)",
        "error:range" => "the range could not be resolved (exit 2 on the command line)",
        _ => "the review failed",
    };
    let unread = match paths {
        None => " Its changed files could not be listed.".to_string(),
        Some([]) => String::new(),
        Some(paths) => format!(
            " It changes {} file(s), none of them reviewed: {}.",
            paths.len(),
            some_of(paths)
        ),
    };
    hook_json(
        event,
        &format!(
            "diffctx did not review {range}: {how}.{unread} Run `diffctx <repo> --diff {range} --mode impact` in that repository before going on."
        ),
        false,
    )
}

/// An inspection whose changed files no edit tool or bracketed call of the
/// session is known to have written, while some of its Bash calls could not
/// be bracketed: whether it wrote them is unknown, so nothing is reviewed.
fn not_attributed(event: Event, trace: &mut Trace) -> String {
    trace.noted = Some("unbracketed");
    hook_json(
        event,
        "diffctx did not review the pending change you just inspected: no edit tool or bracketed Bash call of this session is known to have written the changed files, and some Bash calls of this session could not be bracketed, so whether it wrote them is unknown. Run `diffctx <repo> --diff --mode impact` in that repository before going on.",
        false,
    )
}

fn not_analysed(event: Event, why: Silence, trace: &mut Trace) -> String {
    trace.noted = Some(why);
    let because = match why {
        "error:git" => "git refused a read of the repository (its refs, reflogs or changed files)",
        "error:record" => "what it was about to do could not be recorded for the review after it",
        _ => "git's record of it could not be read",
    };
    hook_json(
        event,
        &format!(
            "diffctx did not analyse this command: {because}. Run `diffctx <repo> --diff --mode impact` in that repository before going on."
        ),
        false,
    )
}

/// The log is cut over to `<log>.1` past this size: tens of thousands of
/// runs, so an audit reads a whole session (#464).
const LOG_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Up to 1.19 the log was `hook.log`, which a pin at or below 1.18.4 still
/// running beside this one cuts at 256 KiB, overwriting what it rotated
/// before: a name no older pin writes keeps this history whole (#555). An
/// audit reads both.
const LOG_NAME: &str = "hook-runs.log";

/// One line per run in `<cache>/diffctx/hook-runs.log` (#338): Claude Code keeps
/// only hooks that print, so without it a silent hook that decided "nothing
/// to say" and one that never got an answer look the same afterwards.
/// `<unix ts> <session> <event> <verb> <range> <outcome> <ms>ms <repo>
/// call=<tool_use_id> roots=<repo,repo> <version>`: the version last, so an
/// audit can tell an old pin's answer from the current one (#524) without
/// moving the fields before it. `call` joins a call's lines with the
/// transcript; `roots` names every repository something landed in (M1).
#[allow(
    clippy::disallowed_methods,
    reason = "the hook's own log: a rotation or line that cannot be written leaves no line, as with a cache nobody can write"
)]
fn log_outcome(event: Event, trace: &Trace, outcome: &str, elapsed: Duration) {
    let Some(dir) = cache_root() else { return };
    let path = dir.join(LOG_NAME);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_MAX_BYTES) {
        let _ = std::fs::rename(&path, dir.join(format!("{LOG_NAME}.1")));
    }
    sweep_seen_daily(&dir.join("seen"));
    let session = file_safe(trace.session.as_deref());
    let line = format!(
        "{} {} {} {} {} {outcome} {}ms {} call={} roots={} {}\n",
        now_secs(),
        if session.is_empty() { "-" } else { &session },
        event.name(),
        trace.verb.unwrap_or("-"),
        trace.range.as_deref().unwrap_or("-"),
        elapsed.as_millis(),
        trace.root.as_deref().map_or_else(
            || "-".to_string(),
            |r| r.to_string_lossy().replace(['\n', '\r'], "?")
        ),
        trace.call.as_deref().unwrap_or("-"),
        if trace.roots.is_empty() {
            "-".to_string()
        } else {
            trace
                .roots
                .iter()
                .map(|r| r.to_string_lossy().replace(['\n', '\r', ' ', ','], "?"))
                .collect::<Vec<_>>()
                .join(",")
        },
        crate::run_provenance::BUILD_LABEL,
    );
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(line.as_bytes()));
}

/// Markers in `seen/` are never read once their TTL has passed, so a
/// long-lived cache otherwise grows without bound (#569).
#[allow(
    clippy::disallowed_methods,
    reason = "the hook's own markers: one that cannot be listed or stat'ed is kept until a later sweep"
)]
fn sweep_seen(seen: &Path) {
    let Ok(entries) = std::fs::read_dir(seen) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let ttl = if entry.file_name().to_string_lossy().starts_with("upgrade-") {
            UPGRADE_NOTICE_TTL
        } else {
            SEEN_TTL
        };
        let expired = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age >= ttl);
        if expired {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The hook's answer, or `None` for silence; every failure path is silence,
/// and every run leaves its line in the hook log.
pub fn respond_within_deadline(event: Event, stdin_json: String, gate: bool) -> Option<String> {
    let _run = crate::profile::run("hook");
    let started = Instant::now();
    let mut trace = Trace::default();
    // The git calls that pick the range run on this thread before any
    // pipeline does; the hook's own context bounds them together.
    let hook_run = crate::resource::RunContext::for_run(HOOK_DEADLINE_SECS);
    let _in_hook = hook_run.enter();
    let answer = decide(event, &stdin_json, gate, started, &mut trace);
    let outcome = match &answer {
        Ok(_) => trace.noted.unwrap_or("shown"),
        Err(why) => why,
    };
    log_outcome(event, &trace, outcome, started.elapsed());
    let session = trace.session.as_deref();
    // The `Err` is the silence reason `log_outcome` wrote just above.
    #[allow(clippy::disallowed_methods, reason = "logged by `log_outcome` above")]
    let answer = match (answer.ok(), pending_review(event, session, started)) {
        (answer, None) => answer,
        (None, late) => late,
        (Some(answer), Some(late)) => joined(vec![late, answer]),
    };
    match (answer, upgrade_notice(session)) {
        (answer, None) => answer,
        (None, Some(notice)) => Some(hook_json(event, &notice, false)),
        (Some(answer), Some(notice)) => Some(with_notice(&answer, &notice)),
    }
}

/// The sweep runs once a day on a stamp of its own. The log's rotation is no
/// clock for it: it comes after tens of thousands of runs.
#[allow(
    clippy::disallowed_methods,
    reason = "the sweep's own stamp: one that cannot be stat'ed runs the sweep again"
)]
fn sweep_seen_daily(seen: &Path) {
    let stamp = seen.join(".swept");
    let fresh = std::fs::metadata(&stamp)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .is_some_and(|age| age < SEEN_TTL);
    if !fresh && std::fs::write(&stamp, "").is_ok() {
        sweep_seen(seen);
    }
}

/// How long a session is told about one newer version: once, for any
/// session that lasts less than this.
const UPGRADE_NOTICE_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// #524: a session runs the plugin it started with, and a version installed
/// since sits beside it in the plugin cache. Claude Code exports the plugin's
/// root to its hooks (`CLAUDE_PLUGIN_ROOT`), so the wrapper forwards nothing;
/// a newer `X.Y.Z` sibling is said once per session.
#[allow(
    clippy::disallowed_methods,
    reason = "advisory: a plugin cache that cannot be listed gives no upgrade notice"
)]
fn upgrade_notice(session: Option<&str>) -> Option<String> {
    let root = PathBuf::from(std::env::var_os("CLAUDE_PLUGIN_ROOT")?);
    let running = env!("CARGO_PKG_VERSION");
    let (newest_key, newest) = std::fs::read_dir(root.parent()?)
        .ok()?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter_map(|name| Some((release_key(&name)?, name)))
        .max()?;
    if newest_key <= release_key(running)? {
        return None;
    }
    let key = session_key(&format!("upgrade-{newest}"), session);
    if is_seen_within(&key, UPGRADE_NOTICE_TTL) {
        return None;
    }
    mark_seen(&key);
    Some(format!(
        "diffctx {newest} is installed; this session runs {running}. Restart the session to pick up its fixes."
    ))
}

/// Orders `X.Y.Z` releases and the development builds before them, in either
/// spelling (`2.0.0.dev0`, `2.0.0-dev.0`), `+source` marks ignored: a dev build
/// sorts after the previous release and before its own (#570).
#[allow(
    clippy::disallowed_methods,
    reason = "a directory name that is no number is no release: None says so"
)]
fn release_key(name: &str) -> Option<(u64, u64, u64, bool, u64)> {
    let name = name.split('+').next()?;
    let (base, dev) = match name.split_once("-dev").or_else(|| name.split_once(".dev")) {
        Some((base, n)) => (base, Some(n.trim_start_matches('.').parse::<u64>().ok()?)),
        None => (name, None),
    };
    let mut parts = base.split('.').map(|p| p.parse::<u64>().ok());
    let (major, minor, patch) = (parts.next()??, parts.next()??, parts.next()??);
    parts
        .next()
        .is_none()
        .then_some((major, minor, patch, dev.is_none(), dev.unwrap_or(0)))
}

fn with_notice(answer: &str, notice: &str) -> String {
    let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(answer) else {
        return answer.to_string();
    };
    let context = &mut doc["hookSpecificOutput"]["additionalContext"];
    let rest = context.as_str().unwrap_or("");
    *context = serde_json::Value::String(format!("{notice}\n\n{rest}"));
    doc.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dev_build_sorts_after_its_previous_release_and_before_its_own() {
        let key = |name: &str| release_key(name).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(key("2.0.0.dev0"), key("2.0.0-dev.0"));
        assert_eq!(key("2.0.0-dev.0"), key("2.0.0-dev0+source"));
        assert!(key("1.20.0") < key("2.0.0-dev.0"));
        assert!(key("2.0.0-dev.0") < key("2.0.0-dev.1"));
        assert!(key("2.0.0.dev7") < key("2.0.0"));
        assert!(key("2.0.0") < key("2.0.1.dev0"));
        for not_a_release in [
            "1000.0",
            "not-a-version",
            "2.0.0.dev",
            "2.0.0-rc.1",
            "1.2.3.4",
        ] {
            assert_eq!(release_key(not_a_release), None, "{not_a_release}");
        }
    }

    fn searched_in(command: &str) -> Option<(Option<String>, Vec<String>)> {
        let payload = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": command}});
        searched(&payload).map(|s| (s.dir, pattern_names(&s.pattern)))
    }

    #[test]
    fn a_search_names_its_pattern_and_where_it_looks() {
        let names = |c: &str| searched_in(c).map(|(_, n)| n);
        assert_eq!(
            names(r#"grep -rn "def handle_upload" ."#),
            Some(vec!["handle_upload".to_string()])
        );
        assert_eq!(
            names(r"rg -n 'ClassName\(' src/"),
            Some(vec!["ClassName".to_string()])
        );
        assert_eq!(
            names(r"git grep -n -e '\btotal\b' -- '*.py'"),
            Some(vec!["total".to_string()])
        );
        assert_eq!(
            names("rg -C 3 -t py 'fn (charge|refund)'"),
            Some(vec!["charge".to_string(), "refund".to_string()])
        );
        assert_eq!(
            names("grep -rn -e total -e 'self.restock(' ."),
            Some(vec!["total".to_string(), "restock".to_string()])
        );
        for line in [
            "grep -rn '^from models import' pkg",
            "git grep -n 'AGENTS.md'",
            "grep -n 'limiter.limit' app.py",
        ] {
            assert_eq!(names(line), Some(Vec::new()), "{line}");
        }
        assert_eq!(
            searched_in("cd /r && rg -n total shop/").unwrap().0,
            Some("/r/shop/".to_string())
        );
        let both = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "cd /r && grep -rn total shop tests"}});
        assert_eq!(searched(&both).unwrap().paths, ["/r/shop", "/r/tests"]);
        let flags = |c: &str| {
            let payload = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": c}});
            searched(&payload).map(|s| (s.inverted, s.revisions))
        };
        assert_eq!(flags("grep -rvn total ."), Some((true, vec![])));
        assert_eq!(flags("git grep -L total"), Some((true, vec![])));
        assert_eq!(
            flags("git grep -n total origin/main -- src"),
            Some((false, vec!["origin/main".to_string()]))
        );
        assert_eq!(flags("rg -n -C 3 total src/"), Some((false, vec![])));
        assert!(searched_in("git log | grep total").is_none());
        assert!(searched_in("cd $R && grep -rn total .").is_none());
        assert!(searched_in("cargo test").is_none());
        let grep_tool = serde_json::json!({"tool_name": "Grep", "tool_input": {"pattern": "class Cart\\b", "path": "/r/src"}});
        let tool = searched(&grep_tool).unwrap();
        assert_eq!(tool.dir.as_deref(), Some("/r/src"));
        assert_eq!(pattern_names(&tool.pattern), vec!["Cart".to_string()]);
    }

    #[test]
    fn the_gate_denies_with_the_impact_attached() {
        let json = hook_json(Event::PreToolUse, "ctx", true);
        let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(doc["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(doc["hookSpecificOutput"]["additionalContext"], "ctx");
        let reason = doc["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(
            reason.contains("ctx"),
            "the denial itself carries the impact: {reason}"
        );
        let open: serde_json::Value =
            serde_json::from_str(&hook_json(Event::PreToolUse, "ctx", false)).unwrap();
        assert!(
            open["hookSpecificOutput"]
                .get("permissionDecision")
                .is_none()
        );
        let after: serde_json::Value = serde_json::from_str(&hook_json(
            Event::PostToolUse { failed: false },
            "ctx",
            true,
        ))
        .unwrap();
        assert!(
            after["hookSpecificOutput"]
                .get("permissionDecision")
                .is_none(),
            "a post-tool hook cannot deny what already ran"
        );
    }

    #[test]
    fn malformed_input_is_silence() {
        let decide = |stdin: &str| {
            decide(
                Event::PreToolUse,
                stdin,
                false,
                Instant::now(),
                &mut Trace::default(),
            )
        };
        assert_eq!(decide("not json"), Err("error:payload"));
        assert_eq!(decide("{}"), Err("no-trigger"));
        assert_eq!(
            decide(r#"{"cwd":"/nonexistent/diffctx","tool_input":{"command":"ls"}}"#),
            Err("no-trigger")
        );
    }
}
