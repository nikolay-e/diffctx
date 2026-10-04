//! `diffctx hook <event>`: the Claude Code hooks that put a change's impact
//! in front of the agent without asking it to call anything (#310).
//!
//! * `pretooluse`: before `git commit`, `merge`, `cherry-pick`, `push` or
//!   `gh pr create`, the impact of what that command is about to record.
//! * `posttooluse`: after a `git diff` or `git status` the agent ran on its
//!   own, the impact of the working tree it just asked about — the path the
//!   agent already walks, with nothing new to decide. After a text search
//!   for a name the pending change edits, that name's callers.
//!
//! Each reads the event on stdin and answers with `additionalContext`, or
//! with nothing when the impact is empty, the same content was already
//! reviewed, or anything at all goes wrong. A hook that blocks a commit
//! because it failed would be worse than no hook; the one deliberate block,
//! the opt-in strict gate, denies with the impact attached so the retry
//! passes.

use std::io::Write as _;

use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::config::limits::{DEFAULT_PPR_ALPHA, DEFAULT_SCORING};
use crate::mode::ScoringMode;

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

/// What the pipeline itself may spend, inside `HOOK_DEADLINE_SECS`: the
/// engine's deadline is cooperative and the line log runs after it, so the
/// gap is what keeps the whole answer under the ceiling.
pub const PIPELINE_SECS: u64 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    PreToolUse,
    PostToolUse,
}

impl Event {
    fn name(self) -> &'static str {
        match self {
            Event::PreToolUse => "PreToolUse",
            Event::PostToolUse => "PostToolUse",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Trigger {
    /// `all` is `-a`/`-am` or an explicit pathspec: the working tree goes
    /// in; a plain `git commit` commits the index and nothing else.
    Commit {
        all: bool,
        /// `--amend` rewrites `HEAD`: what lands is the index (or the tree)
        /// against `HEAD~1`.
        amend: bool,
        /// The target is computed before git runs and may not be what it
        /// records: `-a`, a pathspec, a `git add` earlier on the line.
        preview: bool,
    },
    /// After `git commit`: the commit that landed, reviewed when it is not
    /// what the preview before it showed.
    Committed,
    Merge(String),
    CherryPick(String),
    Push,
    PullRequest,
    /// `git diff` / `git status` the agent ran itself: the working tree
    /// (`--cached`/`--staged` narrows it to the index).
    Inspect {
        staged: bool,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub struct Detected {
    pub trigger: Trigger,
    /// `git -C <dir>` when the command names one.
    pub dir: Option<String>,
}

/// Splits a shell command line into words the way the verbs below are read:
/// quotes group, operators end the statement. Not a shell; enough to find
/// the git or gh statements and their flags.
fn words(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                // A redirection (`2>&1`, `>/dev/null`, `&>log`, `<in`) is
                // the shell's, not the command's: dropped whole, its `&` is
                // not an operator and its target is not a pathspec.
                '>' | '<' => {
                    if !cur.chars().all(|d| d.is_ascii_digit()) {
                        out.push(std::mem::take(&mut cur));
                    }
                    cur.clear();
                    skip_redirection(&mut chars);
                }
                '&' if chars.peek() == Some(&'>') => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                    skip_redirection(&mut chars);
                }
                '\'' | '"' => quote = Some(c),
                '\\' => {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                }
                c if c.is_whitespace() => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                c if OPERATORS.contains(&c) => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                    // `||` and `&&` are one operator each, never a pipe or a
                    // background job; `|&` pipes stderr too.
                    let mut op = c.to_string();
                    if let Some(n) = chars.next_if(|n| *n == c || (c == '|' && *n == '&')) {
                        op.push(n);
                    }
                    out.push(op);
                }
                _ => cur.push(c),
            },
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

const OPERATORS: &[char] = &[';', '|', '&'];

/// The rest of a redirection after its first `>`/`<`: more `>`/`&`, then
/// the target word.
fn skip_redirection(chars: &mut std::iter::Peekable<std::str::Chars>) {
    while chars.next_if(|n| matches!(n, '>' | '&')).is_some() {}
    while chars.next_if(|n| n.is_whitespace()).is_some() {}
    while chars
        .next_if(|n| !n.is_whitespace() && !OPERATORS.contains(n))
        .is_some()
    {}
}

fn is_operator(w: &str) -> bool {
    matches!(w, ";" | "|" | "&" | "||" | "&&" | "|&")
}

/// The statement's output goes into another command, which is what the
/// agent then reads: `git diff | grep x` shows the agent grep's lines.
fn is_pipe(w: &str) -> bool {
    matches!(w, "|" | "|&")
}

/// Flags whose next word is a value, not a pathspec or a revision.
const VALUE_FLAGS: &[&str] = &[
    "-m",
    "-F",
    "-C",
    "-c",
    "-t",
    "-S",
    "-G",
    "-O",
    "--message",
    "--file",
    "--author",
    "--date",
    "--fixup",
    "--squash",
    "--template",
    "--reuse-message",
    "--reedit-message",
    "--trailer",
    "--cleanup",
    "--pathspec-from-file",
];

/// The positional arguments before `--`, with the values of value-taking
/// flags skipped: `git commit -m a -m b` has none, `git commit -m x a.py`
/// has one.
fn positionals(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
            continue;
        }
        if a == "--" {
            break;
        }
        if VALUE_FLAGS.contains(&a.as_str()) {
            skip = true;
        } else if !a.starts_with('-') {
            out.push(a.as_str());
        }
    }
    out
}

/// One git, gh or search statement of a command line.
struct Statement {
    verb: String,
    dir: Option<String>,
    args: Vec<String>,
    /// Its output feeds the next command of a pipeline.
    piped_out: bool,
}

/// Every git or gh statement of the command line: verb, directory (`-C`, or
/// a `cd` earlier on the line) and the arguments up to the next shell
/// operator. `git add -A && git commit` is two statements, and the second is
/// the one that matters.
fn statements(command: &str) -> Vec<Statement> {
    let ws = words(command);
    let mut found = Vec::new();
    let mut cwd: Option<String> = None;
    // Set once a `cd` names a directory the line alone cannot resolve
    // (`cd $R`, `cd ~/x`); git statements under it are skipped rather than
    // reviewed against whatever repository the agent's cwd happens to be.
    let mut lost = false;
    let mut i = 0;
    while i < ws.len() {
        let prog = Path::new(&ws[i])
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();
        if ws[i] == "cd" {
            if let Some(target) = ws
                .get(i + 1)
                .filter(|w| !is_operator(w) && !w.starts_with('-'))
            {
                if is_unresolvable(target) {
                    lost = true;
                } else if is_absolute_dir(target) || !lost {
                    cwd = Some(join_dir(cwd.as_deref(), target));
                    lost = false;
                }
            }
            i += 1;
            continue;
        }
        if SEARCH_TOOLS.contains(&prog.as_str()) {
            // `… | grep x` filters another command's output, not the code.
            let piped = i > 0 && is_pipe(&ws[i - 1]);
            let mut j = i + 1;
            while j < ws.len() && !is_operator(&ws[j]) {
                j += 1;
            }
            if !piped && !lost {
                found.push(Statement {
                    verb: "grep".to_string(),
                    dir: cwd.clone(),
                    args: ws[i + 1..j].to_vec(),
                    piped_out: ws.get(j).is_some_and(|w| is_pipe(w)),
                });
            }
            i = j;
            continue;
        }
        if prog == "git" || prog == "gh" {
            let mut j = i + 1;
            let mut dir = cwd.clone();
            let mut dir_lost = lost;
            let mut verb = None;
            while j < ws.len() && !is_operator(&ws[j]) {
                let w = &ws[j];
                if prog == "git" && w == "-C" {
                    if let Some(d) = ws.get(j + 1) {
                        if is_unresolvable(d) {
                            dir_lost = true;
                        } else if is_absolute_dir(d) {
                            dir = Some(d.clone());
                            dir_lost = false;
                        } else {
                            dir = Some(join_dir(cwd.as_deref(), d));
                        }
                    }
                    j += 2;
                    continue;
                }
                if prog == "git" && w == "-c" {
                    j += 2;
                    continue;
                }
                if w.starts_with('-') {
                    j += 1;
                    continue;
                }
                verb = Some(w.clone());
                j += 1;
                break;
            }
            let mut args = Vec::new();
            while j < ws.len() && !is_operator(&ws[j]) {
                args.push(ws[j].clone());
                j += 1;
            }
            if let Some(verb) = verb.filter(|_| !dir_lost) {
                found.push(Statement {
                    verb: if prog == "gh" {
                        format!("gh {verb}")
                    } else {
                        verb
                    },
                    dir,
                    args,
                    piped_out: ws.get(j).is_some_and(|w| is_pipe(w)),
                });
            }
            i = j;
            continue;
        }
        i += 1;
    }
    found
}

const SEARCH_TOOLS: &[&str] = &["grep", "egrep", "fgrep", "rg", "ag", "ack"];

/// Search flags whose next word is a value, not the pattern or a path.
const SEARCH_VALUE_FLAGS: &str = "-f --file -A -B -C --context --after-context --before-context -m --max-count -g --glob --iglob -t --type -T --type-not -j --threads -M --max-columns -d --max-depth";

/// The pattern of a grep/rg/ag/`git grep` argument list, and the path after
/// it when there is one.
fn search_pattern(args: &[String]) -> Option<(String, Option<String>)> {
    let mut pattern = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "-e" || a == "--regexp" {
            pattern = it.next().cloned();
        } else if let Some(p) = a.strip_prefix("--regexp=") {
            pattern = Some(p.to_string());
        } else if SEARCH_VALUE_FLAGS.split(' ').any(|f| f == a) {
            it.next();
        } else if a.starts_with('-') {
            // A boolean flag, or `--` before the paths.
        } else if pattern.is_none() {
            pattern = Some(a.clone());
        } else {
            let path = Some(a.clone()).filter(|p| !is_unresolvable(p));
            return Some((pattern?, path));
        }
    }
    Some((pattern?, None))
}

/// Keywords a definition search spells around the name: `def total`,
/// `fn total`, `class Cart`.
const DECLARATION_WORDS: &str = "def class fn func function struct enum trait impl interface type let const var pub async static export import from new self this return";

/// The identifiers a search pattern names, with its regex escapes (`\b`,
/// `\(`, `\w`) and declaration keywords dropped.
fn pattern_names(pattern: &str) -> Vec<String> {
    let mut cleaned = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            chars.next();
            cleaned.push(' ');
        } else if c.is_alphanumeric() || c == '_' {
            cleaned.push(c);
        } else {
            cleaned.push(' ');
        }
    }
    let mut names: Vec<String> = Vec::new();
    for w in cleaned.split_whitespace() {
        if w.chars().count() >= 2
            && !w.starts_with(|c: char| c.is_ascii_digit())
            && !DECLARATION_WORDS.split(' ').any(|d| d == w)
            && !names.iter().any(|n| n == w)
        {
            names.push(w.to_string());
        }
    }
    names.truncate(8);
    names
}

/// A directory only the shell could expand: a variable, a command
/// substitution, a home-relative path.
fn is_unresolvable(dir: &str) -> bool {
    dir.contains('$') || dir.contains('`') || dir.starts_with('~')
}

/// A directory as the shell line spelled it: `/r` is absolute on every
/// platform here, and the join keeps the line's own separator.
fn is_absolute_dir(dir: &str) -> bool {
    dir.starts_with('/') || Path::new(dir).is_absolute()
}

fn join_dir(base: Option<&str>, dir: &str) -> String {
    match base {
        Some(b) if !is_absolute_dir(dir) => format!("{}/{dir}", b.trim_end_matches('/')),
        _ => dir.to_string(),
    }
}

pub fn detect(event: Event, command: &str) -> Option<Detected> {
    // The hook runs before the line does: a `git add` earlier on it has not
    // staged anything yet, so the commit that follows records the working
    // tree, not the index this process can see.
    let mut staged_ahead = false;
    for st in statements(command) {
        if matches!(st.verb.as_str(), "add" | "rm" | "mv") {
            staged_ahead = true;
        }
        if let Some(mut detected) = detect_statement(event, &st) {
            if staged_ahead {
                if let Trigger::Commit { all, preview, .. } = &mut detected.trigger {
                    *all = true;
                    *preview = true;
                }
            }
            return Some(detected);
        }
    }
    None
}

/// Views that show no patch: what the agent reads is a list of names or
/// counts, not the change. `--patch` (or `-p`, `-u`) puts the patch back;
/// `--exit-code` changes only the status.
const SUMMARY_FLAGS: &[&str] = &[
    "--stat",
    "--numstat",
    "--shortstat",
    "--name-only",
    "--name-status",
    "--dirstat",
    "--summary",
    "--compact-summary",
    "--quiet",
    "-s",
    "--no-patch",
];

fn is_summary_only(args: &[String]) -> bool {
    let summary = args.iter().any(|a| {
        SUMMARY_FLAGS
            .iter()
            .any(|f| a == f || a.starts_with(&format!("{f}=")))
    });
    let patch = args
        .iter()
        .any(|a| matches!(a.as_str(), "-p" | "-u" | "--patch" | "--patch-with-stat"));
    summary && !patch
}

fn detect_statement(event: Event, st: &Statement) -> Option<Detected> {
    let (verb, args) = (st.verb.as_str(), &st.args);
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let positional = || args.iter().find(|a| !a.starts_with('-')).cloned();
    let trigger = match (event, verb) {
        (Event::PreToolUse, "commit") => {
            if has("--dry-run") {
                return None;
            }
            let short_a = args
                .iter()
                .any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('a'));
            let pathspec = !positionals(args).is_empty() || has("--");
            let all = has("--all") || short_a || pathspec;
            Trigger::Commit {
                all,
                amend: has("--amend"),
                // What `-a` or a pathspec records is decided when git runs;
                // computed now, from the working tree, it is a preview.
                preview: all,
            }
        }
        (Event::PostToolUse, "commit") => {
            if has("--dry-run") {
                return None;
            }
            Trigger::Committed
        }
        (Event::PreToolUse, "merge" | "cherry-pick") => {
            if ["--abort", "--continue", "--skip", "--quit"]
                .iter()
                .any(|f| has(f))
            {
                return None;
            }
            let target = positional()?;
            if verb == "merge" {
                Trigger::Merge(target)
            } else {
                Trigger::CherryPick(target)
            }
        }
        (Event::PreToolUse, "push") => {
            if ["--tags", "--delete", "-d", "--dry-run", "-n"]
                .iter()
                .any(|f| has(f))
            {
                return None;
            }
            Trigger::Push
        }
        (Event::PreToolUse, "gh pr") => {
            if args.first().map(String::as_str) != Some("create") {
                return None;
            }
            Trigger::PullRequest
        }
        (Event::PostToolUse, "diff") => {
            // A diff between revisions, or against one other than HEAD, is
            // history, not the pending change; a path before `--` is.
            if st.piped_out || is_summary_only(args) {
                return None;
            }
            let revs = positionals(args);
            let looks_like_path = |r: &str| r.contains('/') || r.starts_with('.');
            if revs.len() > 1
                || revs.iter().any(|r| r.contains(".."))
                || revs.iter().any(|r| *r != "HEAD" && !looks_like_path(r))
            {
                return None;
            }
            Trigger::Inspect {
                staged: has("--cached") || has("--staged"),
            }
        }
        (Event::PostToolUse, "status") if !st.piped_out => Trigger::Inspect { staged: false },
        _ => return None,
    };
    Some(Detected {
        trigger,
        dir: st.dir.clone(),
    })
}

fn git_out(root: &Path, args: &[&str]) -> Option<String> {
    crate::git::run_git(root, args)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn first_reachable(root: &Path, revs: &[&str]) -> Option<String> {
    revs.iter()
        .find(|r| git_out(root, &["rev-parse", "--verify", "--quiet", r]).is_some())
        .map(|r| r.to_string())
}

/// The index as a tree: what a plain `git commit` records, without the
/// unstaged edits beside it.
/// The index captured as a tree against `base`, or against the snapshot's
/// own base (`HEAD`, the empty tree on an unborn branch).
fn staged_range(root: &Path, base: Option<&str>) -> Option<String> {
    let staged = crate::git::capture_staged(root).ok()?;
    Some(format!("{}..{}", base.unwrap_or(&staged.base), staged.tree))
}

/// The range whose impact the tool call is about to make permanent.
pub fn range_for(root: &Path, trigger: &Trigger) -> Option<String> {
    Some(match trigger {
        Trigger::Commit {
            all: true,
            amend: false,
            ..
        }
        | Trigger::Inspect { staged: false } => "HEAD".to_string(),
        Trigger::Commit {
            all: false,
            amend: false,
            ..
        }
        | Trigger::Inspect { staged: true } => staged_range(root, None)?,
        Trigger::Committed => {
            let parent = git_out(root, &["rev-parse", "--verify", "--quiet", "HEAD~1"])
                .unwrap_or_else(|| {
                    crate::git::capture_staged(root).map_or_else(|_| String::new(), |s| s.base)
                });
            if parent.is_empty() {
                return None;
            }
            format!("{parent}..HEAD")
        }
        Trigger::Commit {
            all, amend: true, ..
        } => {
            let parent = git_out(root, &["rev-parse", "--verify", "--quiet", "HEAD~1"])?;
            if *all {
                parent
            } else {
                staged_range(root, Some(&parent))?
            }
        }
        Trigger::Merge(target) => {
            let base = git_out(root, &["merge-base", "HEAD", target])?;
            format!("{base}..{target}")
        }
        Trigger::CherryPick(target) => {
            if target.contains("..") {
                target.clone()
            } else {
                format!("{target}^..{target}")
            }
        }
        Trigger::Push => {
            let base = first_reachable(
                root,
                &["@{upstream}", "origin/HEAD", "origin/main", "origin/master"],
            )?;
            format!("{base}..HEAD")
        }
        Trigger::PullRequest => {
            let base = first_reachable(root, &["origin/HEAD", "origin/main", "origin/master"])?;
            let merge_base = git_out(root, &["merge-base", &base, "HEAD"])?;
            format!("{merge_base}..HEAD")
        }
    })
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
/// base or the analyser changing makes a new one. `None` when the range
/// changes nothing.
pub fn result_identity(root: &Path, range: &str) -> Option<String> {
    let (base, head) = match crate::git::split_diff_range(range) {
        (Some(b), h) => (b, h),
        _ => (range.to_string(), None),
    };
    let mut entries = changed_blobs(root, &base, head.as_deref())?;
    if head.is_none() {
        let untracked =
            crate::git::run_git_z(root, &["ls-files", "-o", "--exclude-standard", "-z"])
                .unwrap_or_default();
        entries.extend(
            untracked
                .into_iter()
                .map(|p| (p, "100644".to_string(), String::new())),
        );
    }
    if entries.is_empty() {
        return None;
    }
    hash_working_tree_blobs(root, &mut entries)?;
    entries.sort();
    // The checkout by its canonical root: `.` from a manual run and the
    // absolute path the hook resolves are one repository.
    let checkout = crate::git::find_toplevel(root).unwrap_or_else(|| root.to_path_buf());
    let base = git_out(root, &["rev-parse", "--verify", "--quiet", &base]).unwrap_or_default();
    let mut h = Fnv::new();
    h.feed(env!("CARGO_PKG_VERSION").as_bytes())
        .feed(checkout.to_string_lossy().as_bytes())
        .feed(base.as_bytes());
    for (path, mode, oid) in &entries {
        h.feed(path.as_bytes())
            .feed(mode.as_bytes())
            .feed(oid.as_bytes());
    }
    Some(format!("{:016x}", h.0))
}

/// `(path, mode, blob id)` of every path `base..head` changes (`head` absent:
/// the working tree, whose blob ids git leaves as zeros), `deleted` for a
/// path the range removes.
fn changed_blobs(
    root: &Path,
    base: &str,
    head: Option<&str>,
) -> Option<Vec<(String, String, String)>> {
    let mut args = vec!["diff", "--raw", "-z", "--no-abbrev", "--no-renames", base];
    args.extend(head);
    args.push("--");
    let raw = crate::git::run_git_z(root, &args).ok()?;
    Some(
        raw.chunks(2)
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
            .collect(),
    )
}

/// Fills the blob ids the working tree leaves open (zeros, untracked files)
/// with what `git add` would store.
fn hash_working_tree_blobs(root: &Path, entries: &mut [(String, String, String)]) -> Option<()> {
    let open = |oid: &str| oid.is_empty() || oid.bytes().all(|b| b == b'0');
    let unhashed: Vec<String> = entries
        .iter()
        .filter(|(_, _, oid)| open(oid))
        .map(|(p, _, _)| p.clone())
        .collect();
    let hashed: FxHashMap<String, String> = unhashed
        .iter()
        .cloned()
        .zip(crate::git::hash_object_paths(root, &unhashed).ok()?)
        .collect();
    for (path, _, oid) in entries.iter_mut().filter(|(_, _, oid)| open(oid)) {
        if let Some(h) = hashed.get(path.as_str()) {
            oid.clone_from(h);
        }
    }
    Some(())
}

fn cache_root() -> Option<PathBuf> {
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

fn seen_headline(key: &str, ttl: Duration) -> Option<String> {
    is_seen_within(key, ttl)
        .then(|| marker(key).and_then(|p| std::fs::read_to_string(p).ok()))
        .map(Option::unwrap_or_default)
}

/// A manual `--mode impact` run on a range counts as that range reviewed:
/// the hooks then stay silent on the commit or push that follows, and the
/// strict gate opens.
pub fn mark_range_reviewed(root: &Path, range: &str, output: &crate::impact::ImpactOutput) {
    if std::env::var_os("DIFFCTX_NO_MARKER").is_some() {
        return;
    }
    if let Some(identity) = result_identity(root, range) {
        let rendered = crate::impact::render_markdown(output);
        mark_delivered(
            &format!("id-{identity}"),
            rendered.lines().next().unwrap_or_default(),
            !output.limits.is_empty(),
        );
    }
}

/// Everything the hook decides before any heavy work: which repository,
/// which range, and what is already known about the answer for it.
struct Prepared {
    root: PathBuf,
    range: String,
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
}

/// What one invocation learned about itself, for its line in the hook log.
#[derive(Default)]
struct Trace {
    session: Option<String>,
    verb: Option<&'static str>,
    root: Option<PathBuf>,
    range: Option<String>,
}

/// Why the hook stayed silent, as its log line names it; `shown` otherwise.
type Silence = &'static str;

fn verb_of(trigger: &Trigger) -> &'static str {
    match trigger {
        Trigger::Commit { .. } => "commit",
        Trigger::Committed => "committed",
        Trigger::Merge(_) => "merge",
        Trigger::CherryPick(_) => "cherry-pick",
        Trigger::Push => "push",
        Trigger::PullRequest => "pr",
        Trigger::Inspect { .. } => "inspect",
    }
}

/// A relative directory on the command line is relative to where the agent
/// ran it, which the payload names; this process's cwd is a guess.
fn repo_root(payload: &serde_json::Value, dir: Option<&str>) -> Option<PathBuf> {
    let base = payload["cwd"]
        .as_str()
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())?;
    let dir = match dir {
        Some(dir) if is_absolute_dir(dir) => PathBuf::from(dir),
        Some(dir) => base.join(dir),
        None => base,
    };
    // A search names a file as often as a directory.
    let dir = if dir.is_file() {
        dir.parent()?.to_path_buf()
    } else {
        dir
    };
    crate::git::find_toplevel(&dir)
}

fn prepare(
    event: Event,
    payload: &serde_json::Value,
    trace: &mut Trace,
) -> Result<Prepared, Silence> {
    let command = payload["tool_input"]["command"]
        .as_str()
        .ok_or("no-trigger")?;
    let detected = detect(event, command).ok_or_else(|| {
        if hides_git(command) {
            "unsupported-syntax"
        } else {
            "no-trigger"
        }
    })?;
    trace.verb = Some(verb_of(&detected.trigger));
    let root = repo_root(payload, detected.dir.as_deref()).ok_or("not-git")?;
    trace.root = Some(root.clone());
    if detected.trigger == Trigger::Committed && !committed_just_now(&root) {
        return Err("no-new-commit");
    }
    let range = range_for(&root, &detected.trigger).ok_or("no-range")?;
    trace.range = Some(range.clone());
    let identity = result_identity(&root, &range).ok_or("clean")?;
    let session = trace.session.as_deref();
    let key = session_key(&format!("id-{identity}"), session);
    let shown =
        delivered(&key, SEEN_TTL).or_else(|| delivered(&format!("id-{identity}"), MANUAL_SEEN_TTL));
    Ok(Prepared {
        root,
        range,
        trigger: detected.trigger,
        record_key: session_key(&format!("rec-{identity}"), session),
        tries_key: session_key(&format!("try-{identity}"), session),
        key,
        shown,
    })
}

/// A git command only the shell could run — in `$(…)`, backticks, `eval`,
/// `sh -c '…'`: what it records cannot be read off the line, and the hook
/// says it did not try rather than guessing.
fn hides_git(command: &str) -> bool {
    let wrapped = command.contains("$(")
        || command.contains('`')
        || command.contains("eval ")
        || command.contains(" -c '")
        || command.contains(" -c \"");
    wrapped
        && ["git commit", "git push", "git merge", "git cherry-pick"]
            .iter()
            .any(|v| command.contains(v))
}

/// `git commit` reports success through the tool, which the hook does not
/// see; a `HEAD` written in the last two minutes is the commit it made.
fn committed_just_now(root: &Path) -> bool {
    git_out(root, &["log", "-1", "--format=%ct", "HEAD"])
        .and_then(|t| t.parse::<u64>().ok())
        .is_some_and(|t| {
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .is_ok_and(|now| now.as_secs().saturating_sub(t) <= 120)
        })
}

fn reminder(headline: &str) -> String {
    let what = if headline.trim().is_empty() {
        "reviewed earlier".to_string()
    } else {
        format!("shown earlier in this session: {}", headline.trim())
    };
    format!(
        "diffctx: this command records a change whose impact was {what}. Those callers still apply; confirm they were checked before going on."
    )
}

fn run_impact(root: &Path, range: &str) -> Result<crate::impact::ImpactOutput, Silence> {
    let scoring = ScoringMode::from_str(DEFAULT_SCORING).map_err(|_| "error:scoring")?;
    crate::pipeline::build_diff_context_impact(
        root,
        Some(range),
        &[],
        DEFAULT_PPR_ALPHA,
        scoring,
        PIPELINE_SECS,
    )
    .map_err(|_| "error:pipeline")
}

struct Answer {
    context: String,
    /// The line a later reminder repeats.
    headline: String,
    /// The run stopped at a limit: delivered as partial, retried later.
    partial: bool,
}

/// The automatic answer's size, limits and notes included. An explicit
/// request keeps the engine's own cap.
const AUTOMATIC_TOKEN_CAP: u32 = 600;

fn lead_of(event: Event, trigger: &Trigger) -> &'static str {
    match (event, trigger) {
        (Event::PreToolUse, Trigger::Commit { preview: true, .. }) => {
            "diffctx previewed what this command will record, computed from the working tree before git runs; the commit is checked again once it lands."
        }
        (Event::PreToolUse, _) => "diffctx reviewed the change this command is about to record.",
        (Event::PostToolUse, Trigger::Committed) => {
            "diffctx reviewed the commit that just landed, which is not what was reviewed before it."
        }
        (Event::PostToolUse, _) => "diffctx reviewed the pending change you just inspected.",
    }
}

fn impact_context(
    event: Event,
    trigger: &Trigger,
    root: &Path,
    range: &str,
) -> Result<Answer, Silence> {
    let mut output = run_impact(root, range)?;
    if output.empty {
        return Err("empty");
    }
    let partial = !output.limits.is_empty();
    let text = crate::impact::render_automatic(&mut output, AUTOMATIC_TOKEN_CAP);
    let headline = text.lines().next().unwrap_or_default().to_string();
    Ok(Answer {
        context: format!(
            "{} What it reaches outside the diff:\n{text}\nCheck these callers before going on, or tell the user why they are unaffected.",
            lead_of(event, trigger)
        ),
        headline,
        partial,
    })
}

/// A text search the agent ran: a Bash grep/rg/ag/`git grep`, or the Grep
/// tool.
struct Searched {
    dir: Option<String>,
    pattern: String,
}

fn searched(payload: &serde_json::Value) -> Option<Searched> {
    if payload["tool_name"] == "Grep" {
        return Some(Searched {
            dir: payload["tool_input"]["path"].as_str().map(str::to_string),
            pattern: payload["tool_input"]["pattern"].as_str()?.to_string(),
        });
    }
    let command = payload["tool_input"]["command"].as_str()?;
    let Statement { dir, args, .. } = statements(command)
        .into_iter()
        .find(|st| st.verb == "grep")?;
    let (pattern, path) = search_pattern(&args)?;
    Some(Searched {
        dir: path.map(|p| join_dir(dir.as_deref(), &p)).or(dir),
        pattern,
    })
}

/// The names of the definitions the pending change edits: the innermost
/// named container around each hunk of the working tree, the way impact
/// names its changed symbols. Only the changed files are parsed, so a
/// search for any other name stops here without running the pipeline.
fn edited_definitions(root: &Path) -> Vec<String> {
    let Ok(hunks) = crate::git::parse_diff(root, Some("HEAD"), &[]) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = hunks.iter().map(|h| PathBuf::from(&*h.path)).collect();
    files.sort();
    files.dedup();
    let fragments = crate::fragmentation::fragment_files(
        &files,
        root,
        &[],
        &mut rustc_hash::FxHashSet::default(),
        None,
        true,
        &crate::resource::RunContext::unbounded(),
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
    names
}

/// The pending change's callers by symbol name, in the impact's own
/// rendering. `partial` is a run that stopped at a limit: a name missing
/// from it is no proof that nothing calls it.
struct Known {
    answers: std::collections::BTreeMap<String, String>,
    partial: bool,
}

fn known_answers(mut output: crate::impact::ImpactOutput) -> Known {
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
                .any(|s| !s.callers.is_empty() || s.unresolved.is_some())
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
    Known { answers, partial }
}

/// A search for a name the pending change defines is a search for its
/// callers, which the impact already has (#337). Once per name per identity
/// per session, marked only once answered; silence for any other name and on
/// a clean tree.
fn search_answer(
    payload: &serde_json::Value,
    search: &Searched,
    started: Instant,
    trace: &mut Trace,
) -> Result<String, Silence> {
    let names = pattern_names(&search.pattern);
    if names.is_empty() {
        return Err("no-symbol");
    }
    let root = repo_root(payload, search.dir.as_deref()).ok_or("not-git")?;
    trace.root = Some(root.clone());
    trace.range = Some("HEAD".to_string());
    let identity = result_identity(&root, "HEAD").ok_or("clean")?;
    let edited = edited_definitions(&root);
    let session = trace.session.clone();
    let key = |name: &str| session_key(&format!("id-{identity}.{name}"), session.as_deref());
    let names: Vec<String> = names.into_iter().filter(|n| edited.contains(n)).collect();
    if names.is_empty() {
        return Err("no-symbol");
    }
    let names: Vec<String> = names
        .into_iter()
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
    let known = within_deadline(started, move || {
        run_impact(&root, "HEAD").map(known_answers)
    })?;
    unmark(&tries);
    let mut answers = Vec::new();
    for name in &names {
        let answer = match known.answers.get(name) {
            Some(text) => format!(
                "You searched for `{name}`, which the pending change edits; diffctx already analysed what references it:\n{text}"
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

/// `work`, bounded: past the deadline the hook prints nothing rather than
/// holding the tool call. A hook that stalls a commit teaches the user to
/// remove it.
fn within_deadline<T: Send + 'static>(
    started: Instant,
    work: impl FnOnce() -> Result<T, Silence> + Send + 'static,
) -> Result<T, Silence> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    let left = Duration::from_secs(HOOK_DEADLINE_SECS).saturating_sub(started.elapsed());
    match rx.recv_timeout(left) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err("timeout"),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err("error:panic"),
    }
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
    let payload: serde_json::Value =
        serde_json::from_str(stdin_json).map_err(|_| "error:payload")?;
    trace.session = payload["session_id"].as_str().map(str::to_string);
    if event == Event::PostToolUse {
        if let Some(search) = searched(&payload) {
            trace.verb = Some("grep");
            return search_answer(&payload, &search, started, trace)
                .map(|c| hook_json(event, &c, false));
        }
    }
    let prepared = prepare(event, &payload, trace)?;
    if event == Event::PreToolUse && is_seen(&prepared.record_key) {
        return Err("seen");
    }
    match (&prepared.shown, event) {
        (Some(d), Event::PreToolUse) if !d.partial => {
            mark_seen(&prepared.record_key);
            return Ok(hook_json(event, &reminder(&d.headline), false));
        }
        (Some(d), Event::PostToolUse) if !d.partial => return Err("seen"),
        _ => {}
    }
    let attempt = attempts(&prepared.tries_key);
    if attempt >= MAX_ATTEMPTS {
        return Err("retry-exhausted");
    }
    mark_seen_with(&prepared.tries_key, &(attempt + 1).to_string());
    let Prepared {
        root,
        range,
        trigger,
        key,
        record_key,
        tries_key,
        ..
    } = prepared;
    let answer = within_deadline(started, move || {
        impact_context(event, &trigger, &root, &range)
    });
    match &answer {
        // Nothing to say is a complete answer, delivered as silence.
        Err("empty") => mark_delivered(&key, "", false),
        Ok(a) => mark_delivered(&key, &a.headline, a.partial),
        Err(_) => return answer.map(|_| String::new()),
    }
    // A partial answer keeps its attempt: it is retried, but not forever.
    if !matches!(&answer, Ok(a) if a.partial) {
        unmark(&tries_key);
    }
    if event == Event::PreToolUse {
        mark_seen(&record_key);
    }
    answer.map(|a| hook_json(event, &a.context, gate && !a.partial))
}

/// The log is cut over to `hook.log.1` past this size: a few thousand runs.
const LOG_MAX_BYTES: u64 = 256 * 1024;

/// One line per run in `<cache>/diffctx/hook.log` (#338): Claude Code keeps
/// only hooks that print, so without it a silent hook that decided "nothing
/// to say" and one that never got an answer look the same afterwards.
/// `<unix ts> <session> <event> <verb> <range> <outcome> <ms>ms <repo>`.
fn log_outcome(event: Event, trace: &Trace, outcome: &str, elapsed: Duration) {
    let Some(dir) = cache_root() else { return };
    let path = dir.join("hook.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_MAX_BYTES) {
        let _ = std::fs::rename(&path, dir.join("hook.log.1"));
    }
    let session = file_safe(trace.session.as_deref());
    let line = format!(
        "{} {} {} {} {} {outcome} {}ms {}\n",
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        if session.is_empty() { "-" } else { &session },
        event.name(),
        trace.verb.unwrap_or("-"),
        trace.range.as_deref().unwrap_or("-"),
        elapsed.as_millis(),
        trace.root.as_deref().map_or_else(
            || "-".to_string(),
            |r| r.to_string_lossy().replace(['\n', '\r'], "?")
        ),
    );
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(line.as_bytes()));
}

/// The hook's answer, or `None` for silence; every failure path is silence,
/// and every run leaves its line in the hook log.
pub fn respond_within_deadline(event: Event, stdin_json: String, gate: bool) -> Option<String> {
    let started = Instant::now();
    let mut trace = Trace::default();
    // The git calls that pick the range run on this thread before any
    // pipeline does; the hook's own context bounds them together.
    let hook_run = crate::resource::RunContext::for_run(HOOK_DEADLINE_SECS);
    let _in_hook = hook_run.enter();
    let answer = decide(event, &stdin_json, gate, started, &mut trace);
    log_outcome(
        event,
        &trace,
        answer.as_ref().err().copied().unwrap_or("shown"),
        started.elapsed(),
    );
    answer.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pre(command: &str) -> Option<Trigger> {
        detect(Event::PreToolUse, command).map(|d| d.trigger)
    }

    fn post(command: &str) -> Option<Trigger> {
        detect(Event::PostToolUse, command).map(|d| d.trigger)
    }

    #[test]
    fn a_redirection_is_neither_a_pathspec_nor_an_operator() {
        for command in [
            "git commit -q -m x 2>&1 | tail -3",
            "git commit -m x >/dev/null 2>&1",
            "git commit -m x &> log.txt",
            "git commit -m x 2> err.txt; git log -1",
        ] {
            assert_eq!(
                pre(command),
                Some(Trigger::Commit {
                    all: false,
                    amend: false,
                    preview: false,
                }),
                "{command}"
            );
        }
        assert_eq!(
            pre("git commit -m x src/a.py 2>&1"),
            Some(Trigger::Commit {
                all: true,
                amend: false,
                preview: true,
            })
        );
    }

    #[test]
    fn verbs_and_their_escapes() {
        assert_eq!(
            pre("git commit -m 'x'"),
            Some(Trigger::Commit {
                all: false,
                amend: false,
                preview: false,
            })
        );
        assert_eq!(
            pre("git commit -am x"),
            Some(Trigger::Commit {
                all: true,
                amend: false,
                preview: true,
            })
        );
        assert_eq!(
            pre("git commit --all -m x"),
            Some(Trigger::Commit {
                all: true,
                amend: false,
                preview: true,
            })
        );
        assert_eq!(
            pre("git commit -m x -- src/a.py"),
            Some(Trigger::Commit {
                all: true,
                amend: false,
                preview: true,
            })
        );
        assert_eq!(
            detect(Event::PreToolUse, "cd repo && git -C /tmp/r commit -am x").unwrap(),
            Detected {
                trigger: Trigger::Commit {
                    all: true,
                    amend: false,
                    preview: true,
                },
                dir: Some("/tmp/r".to_string())
            }
        );
        assert_eq!(
            pre("git commit -q --amend --no-edit"),
            Some(Trigger::Commit {
                all: false,
                amend: true,
                preview: false,
            })
        );
        assert!(pre("git commit --dry-run").is_none());
        assert_eq!(
            pre("git merge feature/x"),
            Some(Trigger::Merge("feature/x".to_string()))
        );
        assert!(pre("git merge --abort").is_none());
        assert_eq!(
            pre("git cherry-pick abc123"),
            Some(Trigger::CherryPick("abc123".to_string()))
        );
        assert!(pre("git cherry-pick --continue").is_none());
        assert_eq!(pre("git push origin main"), Some(Trigger::Push));
        assert!(pre("git push --tags").is_none());
        assert!(pre("git push --dry-run").is_none());
        assert_eq!(pre("gh pr create --fill"), Some(Trigger::PullRequest));
        assert!(pre("gh pr view 12").is_none());
        assert!(pre("git status && git log").is_none());
        // The add has not run when the hook sees the line: the commit
        // records the working tree, not the index of this moment.
        assert_eq!(
            pre("git add -A && git commit -m x && git push"),
            Some(Trigger::Commit {
                all: true,
                amend: false,
                preview: true,
            })
        );
        assert_eq!(
            pre("git commit -m a -m b"),
            Some(Trigger::Commit {
                all: false,
                amend: false,
                preview: false,
            })
        );
        assert_eq!(
            pre("git commit -F msg.txt"),
            Some(Trigger::Commit {
                all: false,
                amend: false,
                preview: false,
            })
        );
        assert_eq!(
            pre("git commit -m x src/a.py"),
            Some(Trigger::Commit {
                all: true,
                amend: false,
                preview: true,
            })
        );
        assert_eq!(
            detect(Event::PreToolUse, "cd sub/app && git commit -m x").unwrap(),
            Detected {
                trigger: Trigger::Commit {
                    all: false,
                    amend: false,
                    preview: false,
                },
                dir: Some("sub/app".to_string())
            }
        );
        assert_eq!(
            detect(Event::PreToolUse, "cd /r && git -C sub commit -m x")
                .unwrap()
                .dir,
            Some("/r/sub".to_string())
        );
        // A directory only the shell can expand is not guessed at: the
        // agent's cwd may be another repository entirely.
        assert!(pre("cd $R && git commit -qm init").is_none());
        assert!(pre("cd ~/x && git commit -m x").is_none());
        assert!(pre("git -C \"$REPO\" commit -m x").is_none());
        assert!(pre("cd $R && cd sub && git commit -m x").is_none());
        assert_eq!(
            detect(Event::PreToolUse, "cd $R && cd /abs && git commit -m x")
                .unwrap()
                .dir,
            Some("/abs".to_string())
        );
    }

    #[test]
    fn the_inspection_verbs_fire_only_after_the_tool_ran() {
        assert_eq!(post("git diff"), Some(Trigger::Inspect { staged: false }));
        assert_eq!(
            post("git diff --cached"),
            Some(Trigger::Inspect { staged: true })
        );
        assert_eq!(
            post("git status --short"),
            Some(Trigger::Inspect { staged: false })
        );
        assert!(post("git diff HEAD~3..HEAD").is_none());
        assert!(post("git diff HEAD~1 HEAD").is_none());
        assert!(post("git diff main").is_none());
        assert_eq!(
            post("git diff HEAD"),
            Some(Trigger::Inspect { staged: false })
        );
        assert_eq!(
            post("git diff -- src/"),
            Some(Trigger::Inspect { staged: false })
        );
        assert_eq!(
            post("git diff src/a.py"),
            Some(Trigger::Inspect { staged: false })
        );
        assert_eq!(post("git commit -m x"), Some(Trigger::Committed));
        assert!(pre("git diff").is_none());
    }

    #[test]
    fn the_first_statement_wins_and_operators_end_it() {
        assert_eq!(
            pre("git add -A; git commit -m 'a; b' | cat"),
            Some(Trigger::Commit {
                all: true,
                amend: false,
                preview: true,
            })
        );
        assert!(pre("git log | grep commit").is_none());
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
            searched_in("cd /r && rg -n total shop/").unwrap().0,
            Some("/r/shop/".to_string())
        );
        assert!(searched_in("git log | grep total").is_none());
        assert!(searched_in("cd $R && grep -rn total .").is_none());
        assert!(searched_in("cargo test").is_none());
        let grep_tool = serde_json::json!({"tool_name": "Grep", "tool_input": {"pattern": "class Cart\\b", "path": "/r/src"}});
        let tool = searched(&grep_tool).unwrap();
        assert_eq!(tool.dir.as_deref(), Some("/r/src"));
        assert_eq!(pattern_names(&tool.pattern), vec!["Cart".to_string()]);
    }

    #[test]
    fn a_cherry_pick_range_is_kept_verbatim() {
        let root = Path::new(".");
        assert_eq!(
            range_for(root, &Trigger::CherryPick("a..b".into())).unwrap(),
            "a..b"
        );
        assert_eq!(
            range_for(root, &Trigger::CherryPick("abc".into())).unwrap(),
            "abc^..abc"
        );
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
        let after: serde_json::Value =
            serde_json::from_str(&hook_json(Event::PostToolUse, "ctx", true)).unwrap();
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
            decide(r#"{"tool_input":{"command":"ls"}}"#),
            Err("no-trigger")
        );
    }
}
