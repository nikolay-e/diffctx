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
    Commit {
        /// `--amend` rewrites `HEAD`: what lands is measured against `HEAD~1`.
        amend: bool,
        staging: Staging,
        /// The `git add` / `git rm` statements earlier on the line, which have
        /// not run when the hook does: replayed on a copy of the index.
        steps: Vec<Replay>,
    },
    /// After `git commit`: what the line landed on `onto` — the `HEAD` its
    /// `PreToolUse` recorded, or that commit's parent for an amend — reviewed
    /// when it is not what the preview before it showed. `onto` is read off
    /// the record once the repository is known (#406).
    Committed {
        amend: bool,
        onto: Option<String>,
    },
    Merge(String),
    /// Every commit the line picks, in order; a range stays one entry.
    CherryPick(Vec<String>),
    Push(Pushed),
    /// `gh pr create`: `head` (the current branch unless `--head` names
    /// one) against `base` (the remote's default unless `--base` names one).
    PullRequest {
        base: Option<String>,
        head: Option<String>,
    },
    /// `git diff` / `git status` the agent ran itself: the working tree
    /// (`--cached`/`--staged` narrows it to the index).
    Inspect {
        staged: bool,
    },
}

/// What `git commit` takes from the index at the moment it starts.
#[derive(Debug, PartialEq, Eq)]
pub enum Staging {
    /// A plain commit: the index, with the line's own staging applied.
    Index,
    /// `-a`: every tracked change on top (`git add -u`).
    All,
    /// A pathspec: those paths as they stand, on top of `HEAD` (`--only`).
    Only(Vec<String>),
    /// `-i <paths>`: those paths on top of the index.
    Include(Vec<String>),
    /// A line the index cannot be planned for (`git mv`, an interactive
    /// add): the working tree, as an estimate.
    Worktree,
}

/// The one ref a `git push` sends, as its line names it (#416).
#[derive(Debug, PartialEq, Eq)]
pub enum Pushed {
    /// `git push` alone: the current branch, onto its upstream.
    Upstream,
    /// `<src>[:<dst>]` onto `remote`; without `dst`, `src`'s own branch.
    Ref {
        remote: String,
        src: String,
        dst: Option<String>,
    },
    /// A variable, a glob, several refs, `--all`: logged as unresolved,
    /// never stood in for by the current branch.
    Unresolved,
}

/// A staging statement as the line spells it, and the directory it runs in.
#[derive(Debug, PartialEq, Eq)]
pub struct Replay {
    pub dir: Option<String>,
    pub args: Vec<String>,
}

impl Trigger {
    /// Built before git runs from what the line will stage, not read off
    /// the index as it stands.
    fn is_preview(&self) -> bool {
        matches!(self, Self::Commit { staging, steps, .. }
            if *staging != Staging::Index || !steps.is_empty())
    }
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
    // Here-documents opened on the current line: their bodies start at the
    // next newline and are text, never words (a commit message's `;`, `'`
    // or `|` would otherwise split or open the statement).
    let mut heredocs: Vec<(String, bool)> = Vec::new();
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            // Inside `"…"` a `$(…)` is a command of its own: its quotes and
            // here-document body do not close the outer string (#405, the
            // usual `-m "$(cat <<'EOF' … EOF)"` with an odd `"` in it).
            Some('"') if c == '$' && chars.peek() == Some(&'(') => {
                cur.push(c);
                if let Some(open) = chars.next() {
                    cur.push(open);
                }
                skip_substitution(&mut chars, &mut cur);
            }
            Some(q) => {
                if c == q {
                    quote = None;
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                '<' if chars.peek() == Some(&'<') => {
                    chars.next();
                    if !cur.chars().all(|d| d.is_ascii_digit()) {
                        out.push(std::mem::take(&mut cur));
                    }
                    cur.clear();
                    if chars.next_if_eq(&'<').is_some() {
                        while chars.next_if(|n| *n == ' ' || *n == '\t').is_some() {}
                        shell_word(&mut chars);
                    } else {
                        let strip_tabs = chars.next_if_eq(&'-').is_some();
                        while chars.next_if(|n| *n == ' ' || *n == '\t').is_some() {}
                        heredocs.push((shell_word(&mut chars), strip_tabs));
                    }
                }
                // A redirection (`2>&1`, `>/dev/null`, `&>log`, `<in`) is
                // the shell's, not the command's: dropped whole, its `&` is
                // not an operator and its target is not a pathspec.
                '>' | '<' => {
                    let fd = std::mem::take(&mut cur);
                    if !fd.chars().all(|d| d.is_ascii_digit()) {
                        out.push(fd.clone());
                    }
                    let to_stdout = c == '>' && (fd.is_empty() || fd == "1");
                    let duplicated = skip_redirection(&mut chars);
                    if to_stdout && !duplicated {
                        out.push(STDOUT_TO_FILE.to_string());
                    }
                }
                '&' if chars.peek() == Some(&'>') => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                    if !skip_redirection(&mut chars) {
                        out.push(STDOUT_TO_FILE.to_string());
                    }
                }
                '\'' | '"' => quote = Some(c),
                '\\' => {
                    // A backslash-newline joins the lines; it is no word.
                    if let Some(n) = chars.next().filter(|n| *n != '\n') {
                        cur.push(n);
                    }
                }
                // A newline ends the statement like `;` does.
                '\n' => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                    out.push(";".to_string());
                    for (delimiter, strip_tabs) in std::mem::take(&mut heredocs) {
                        skip_heredoc_body(&mut chars, &delimiter, strip_tabs);
                    }
                }
                c if c.is_whitespace() => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                // A subshell's parentheses are words of their own: `(cd sub
                // && git add a.py)` is a `cd` and an add of `a.py`, not of
                // `a.py)`. `$(` stays inside its word.
                '(' if cur.is_empty() => out.push("(".to_string()),
                // A `#` opening a word comments out the rest of the line:
                // `# Don't forget` must not open a quote (#405).
                '#' if cur.is_empty() => while chars.next_if(|n| *n != '\n').is_some() {},
                ')' => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                    out.push(")".to_string());
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

/// The rest of a `$(…)` after its `(`, appended to `word` up to and including
/// the `)` that closes it: quoted strings and here-document bodies are read
/// as text, so a `)` or `"` inside them closes nothing.
fn skip_substitution(chars: &mut std::iter::Peekable<std::str::Chars>, word: &mut String) {
    let mut depth = 1usize;
    let mut heredocs: Vec<(String, bool)> = Vec::new();
    while let Some(c) = chars.next() {
        word.push(c);
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    word.push(n);
                }
            }
            '\'' | '"' => {
                while let Some(n) = chars.next() {
                    word.push(n);
                    if n == c {
                        break;
                    }
                    if c == '"' && n == '\\' {
                        if let Some(e) = chars.next() {
                            word.push(e);
                        }
                    }
                }
            }
            '<' if chars.peek() == Some(&'<') => {
                word.push(chars.next().unwrap_or('<'));
                if chars.peek() == Some(&'<') {
                    continue;
                }
                let strip_tabs = chars.next_if_eq(&'-').is_some();
                while chars.next_if(|n| *n == ' ' || *n == '\t').is_some() {}
                let delimiter = shell_word(chars);
                word.push_str(&delimiter);
                heredocs.push((delimiter, strip_tabs));
            }
            '\n' => {
                for (delimiter, strip_tabs) in std::mem::take(&mut heredocs) {
                    while let Some(n) = chars.next() {
                        let mut line = String::from(n);
                        if n != '\n' {
                            while let Some(m) = chars.next_if(|m| *m != '\n') {
                                line.push(m);
                            }
                            chars.next();
                        }
                        word.push_str(&line);
                        word.push('\n');
                        let body = if strip_tabs {
                            line.trim_start_matches('\t')
                        } else {
                            line.as_str()
                        };
                        if body == delimiter {
                            break;
                        }
                    }
                }
            }
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return;
                }
            }
            _ => {}
        }
    }
}

/// One shell word with its quotes removed: a here-document delimiter
/// (`'EOF'`, `"EOF"`, `\EOF`) or a here-string.
fn shell_word(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut word = String::new();
    let mut quote: Option<char> = None;
    while let Some(&c) = chars.peek() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == '\\' => {
                chars.next();
                if let Some(n) = chars.next() {
                    word.push(n);
                }
                continue;
            }
            None if c.is_whitespace() || OPERATORS.contains(&c) || c == '<' || c == '>' => break,
            None => word.push(c),
        }
        chars.next();
    }
    word
}

/// Consumes a here-document body up to and including its delimiter line.
fn skip_heredoc_body(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    delimiter: &str,
    strip_tabs: bool,
) {
    loop {
        let mut line = String::new();
        let mut ended = true;
        for c in chars.by_ref() {
            if c == '\n' {
                ended = false;
                break;
            }
            line.push(c);
        }
        let line = if strip_tabs {
            line.trim_start_matches('\t')
        } else {
            &line
        };
        if line == delimiter || ended {
            return;
        }
    }
}

/// The rest of a redirection after its first `>`/`<`: more `>`/`&`, then
/// the target word. `true` when it duplicates another descriptor (`>&2`),
/// which the tool still shows.
fn skip_redirection(chars: &mut std::iter::Peekable<std::str::Chars>) -> bool {
    let mut duplicate = false;
    while let Some(c) = chars.next_if(|n| matches!(n, '>' | '&')) {
        duplicate |= c == '&';
    }
    while chars.next_if(|n| n.is_whitespace()).is_some() {}
    while chars
        .next_if(|n| !n.is_whitespace() && !OPERATORS.contains(n))
        .is_some()
    {}
    duplicate
}

/// Marks a statement whose output goes to a file: what the agent reads is
/// not the diff, so it is no inspection.
const STDOUT_TO_FILE: &str = "\u{1}>file";

fn is_operator(w: &str) -> bool {
    matches!(w, ";" | "|" | "&" | "||" | "&&" | "|&" | "(" | ")")
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
        if takes_value(a) {
            skip = true;
        } else if !a.starts_with('-') {
            out.push(a.as_str());
        }
    }
    out
}

/// A flag whose value is the next word: a listed one, or a cluster of short
/// flags ending in one (`-am msg`).
fn takes_value(arg: &str) -> bool {
    VALUE_FLAGS.contains(&arg)
        || arg.strip_prefix('-').is_some_and(|cluster| {
            cluster.len() > 1
                && cluster.chars().all(|c| c.is_ascii_alphabetic())
                && cluster.ends_with(['m', 'F', 'C', 'c', 't'])
        })
}

/// The short flags a command spells, clustered or not, values skipped.
fn short_flags(args: &[String]) -> String {
    let mut flags = String::new();
    let mut skip = false;
    for a in args {
        if std::mem::take(&mut skip) {
            continue;
        }
        if a == "--" {
            break;
        }
        skip = takes_value(a);
        if let Some(cluster) = a.strip_prefix('-').filter(|c| !c.starts_with('-')) {
            flags.extend(cluster.chars().take_while(char::is_ascii_alphabetic));
        }
    }
    flags
}

/// Value flags of `merge` / `cherry-pick` beyond the shared ones: the `1`
/// of `cherry-pick -m 1 X` is a parent number, not a commit.
const MERGE_VALUE_FLAGS: &[&str] = &["-s", "-X", "--strategy", "--strategy-option", "--mainline"];

fn positionals_skipping(args: &[String], extra: &[&str]) -> Vec<String> {
    let kept: Vec<String> = args
        .iter()
        .scan(false, |skip, a| {
            if std::mem::take(skip) {
                return Some(None);
            }
            *skip = extra.contains(&a.as_str());
            Some(if *skip { None } else { Some(a.clone()) })
        })
        .flatten()
        .collect();
    positionals(&kept).into_iter().map(str::to_string).collect()
}

/// One git, gh or search statement of a command line.
struct Statement {
    verb: String,
    dir: Option<String>,
    args: Vec<String>,
    /// Its output feeds the next command of a pipeline.
    piped_out: bool,
    /// Runs in a directory only the shell could name (`cd $R`): kept so the
    /// log can say why the line got no answer, never reviewed.
    lost: bool,
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
    // A subshell's `cd` ends with it.
    let mut outer: Vec<(Option<String>, bool)> = Vec::new();
    let mut i = 0;
    while i < ws.len() {
        if ws[i] == "(" {
            outer.push((cwd.clone(), lost));
            i += 1;
            continue;
        }
        if ws[i] == ")" {
            if let Some((dir, was_lost)) = outer.pop() {
                cwd = dir;
                lost = was_lost;
            }
            i += 1;
            continue;
        }
        let prog = Path::new(&ws[i])
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();
        if matches!(ws[i].as_str(), "cd" | "pushd" | "popd") {
            // `cd -`, `cd` alone and `popd` go where the line does not say;
            // `--` and `-P`/`-L` only precede the directory (#405).
            let mut k = i + 1;
            while ws
                .get(k)
                .is_some_and(|w| matches!(w.as_str(), "--" | "-P" | "-L" | "-e" | "-@"))
            {
                k += 1;
            }
            let target = ws
                .get(k)
                .filter(|w| !is_operator(w))
                .cloned()
                .or_else(|| (ws[i] == "cd").then(|| "~".to_string()));
            let target = target.as_ref();
            if ws[i] == "popd" || target.is_none_or(|w| w.starts_with('-')) {
                lost = true;
                i = k;
                continue;
            }
            if let Some(target) = target.map(|w| expand_home(w)) {
                let target = &target;
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
                    args: ws[i + 1..j]
                        .iter()
                        .filter(|w| *w != STDOUT_TO_FILE)
                        .cloned()
                        .collect(),
                    piped_out: ws.get(j).is_some_and(|w| is_pipe(w)),
                    lost: false,
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
                    if let Some(d) = ws.get(j + 1).map(|d| expand_home(d)) {
                        let d = &d;
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
            let mut to_file = false;
            while j < ws.len() && !is_operator(&ws[j]) {
                if ws[j] == STDOUT_TO_FILE {
                    to_file = true;
                } else {
                    args.push(ws[j].clone());
                }
                j += 1;
            }
            if let Some(verb) = verb {
                found.push(Statement {
                    verb: if prog == "gh" {
                        format!("gh {verb}")
                    } else {
                        verb
                    },
                    dir,
                    args,
                    piped_out: to_file || ws.get(j).is_some_and(|w| is_pipe(w)),
                    lost: dir_lost,
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
            let path = Some(expand_home(a)).filter(|p| !is_unresolvable(p));
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
/// substitution, another user's home (`~user`).
fn is_unresolvable(dir: &str) -> bool {
    dir.contains('$') || dir.contains('`') || dir.starts_with('~')
}

/// `~` and `~/…` as the shell expands them, with this process's `HOME`:
/// `cd ~/repo && git commit` is the most common way an agent names a
/// repository (#377).
fn expand_home(dir: &str) -> String {
    let rest = if dir == "~" {
        Some("")
    } else {
        dir.strip_prefix("~/")
    };
    match (rest, std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => {
            let home = home.to_string_lossy();
            let home = home.trim_end_matches('/');
            if rest.is_empty() {
                home.to_string()
            } else {
                format!("{home}/{rest}")
            }
        }
        _ => dir.to_string(),
    }
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
    // staged anything yet. It is replayed on a copy of the index, so the
    // commit that follows is reviewed for what it will start from.
    let mut steps = Vec::new();
    let mut unplannable = false;
    let mut summary = None;
    for st in statements(command).into_iter().filter(|st| !st.lost) {
        match st.verb.as_str() {
            "add" | "stage" if !st.args.iter().any(|a| is_interactive_add(a)) => {
                steps.push(Replay {
                    dir: st.dir.clone(),
                    args: std::iter::once("add".to_string())
                        .chain(st.args.iter().cloned())
                        .collect(),
                });
            }
            // `git rm` also deletes the file; on the copy only the index entry goes.
            "rm" => steps.push(Replay {
                dir: st.dir.clone(),
                args: ["rm", "--cached", "-q"]
                    .into_iter()
                    .map(str::to_string)
                    .chain(st.args.iter().cloned())
                    .collect(),
            }),
            // Anything else that writes the index or the files an add reads.
            "add" | "stage" | "mv" | "reset" | "restore" | "checkout" | "stash" | "apply"
            | "am" | "update-index" | "read-tree" | "checkout-index" | "switch" | "merge"
            | "cherry-pick" | "revert" | "pull" | "rebase" | "clean" => {
                unplannable = true;
            }
            _ => {}
        }
        if let Some(mut detected) = detect_statement(event, &st) {
            // `git status` only summarises: a diff later on the same line is
            // what the agent read, the index alone for `--cached`.
            if st.verb == "status" {
                summary.get_or_insert(detected);
                continue;
            }
            if let Trigger::Commit {
                staging,
                steps: planned,
                ..
            } = &mut detected.trigger
            {
                if unplannable {
                    *staging = Staging::Worktree;
                } else {
                    *planned = std::mem::take(&mut steps);
                }
            }
            return Some(detected);
        }
    }
    summary
}

/// An add that asks the terminal which hunks to take: its outcome cannot
/// be planned.
fn is_interactive_add(arg: &str) -> bool {
    matches!(
        arg,
        "-p" | "--patch" | "-i" | "--interactive" | "-e" | "--edit" | "--pathspec-from-file"
    ) || arg.starts_with("--pathspec-from-file=")
}

/// Every pathspec of a commit: the positionals, and whatever follows `--`.
fn pathspecs(args: &[String]) -> Vec<String> {
    let mut paths: Vec<String> = positionals(args).into_iter().map(str::to_string).collect();
    if let Some(i) = args.iter().position(|a| a == "--") {
        paths.extend(args[i + 1..].iter().cloned());
    }
    paths
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

/// Value flags of `git push`: the next word is no repository or refspec.
const PUSH_VALUE_FLAGS: &[&str] = &["-o", "--push-option", "--repo", "--receive-pack", "--exec"];

/// The ref a push line sends: its repository and its one refspec that does
/// not delete. `None` when every refspec deletes.
fn pushed(args: &[String]) -> Option<Pushed> {
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "--all" | "--branches" | "--mirror"))
    {
        return Some(Pushed::Unresolved);
    }
    let words = positionals_skipping(args, PUSH_VALUE_FLAGS);
    let Some((remote, refspecs)) = words.split_first() else {
        return Some(Pushed::Upstream);
    };
    let sent: Vec<&str> = refspecs
        .iter()
        .map(|r| r.strip_prefix('+').unwrap_or(r))
        .filter(|r| !r.starts_with(':'))
        .collect();
    let spec = match sent.as_slice() {
        [] if refspecs.is_empty() => "HEAD",
        [] => return None,
        [one] => one,
        _ => return Some(Pushed::Unresolved),
    };
    let (src, dst) = match spec.split_once(':') {
        Some((src, dst)) => (src, Some(dst)),
        None => (spec, None),
    };
    let unnamed = |w: &str| w.is_empty() || w.contains('*') || is_unresolvable(w);
    if unnamed(remote) || unnamed(src) || dst.is_some_and(unnamed) {
        return Some(Pushed::Unresolved);
    }
    Some(Pushed::Ref {
        remote: remote.clone(),
        src: src.to_string(),
        dst: dst.map(str::to_string),
    })
}

/// The value of `--long v`, `--long=v`, `-s v` or `-sv`.
fn flag_value(args: &[String], long: &str, short: &str) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == long || a == short {
            return it.next().cloned();
        }
        let attached = a
            .strip_prefix(long)
            .and_then(|v| v.strip_prefix('='))
            .or_else(|| a.strip_prefix(short).filter(|v| !v.is_empty()));
        if let Some(v) = attached {
            return Some(v.to_string());
        }
    }
    None
}

fn detect_statement(event: Event, st: &Statement) -> Option<Detected> {
    let (verb, args) = (st.verb.as_str(), &st.args);
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let short_flag = |c: char| short_flags(args).contains(c);
    let trigger = match (event, verb) {
        (Event::PreToolUse, "commit") => {
            if has("--dry-run") {
                return None;
            }
            // `-S` signs: its key id is attached, never the next word.
            let unsigned: Vec<String> = args.iter().filter(|a| *a != "-S").cloned().collect();
            let paths = pathspecs(&unsigned);
            let staging = if !paths.is_empty() {
                if has("--include") || short_flag('i') {
                    Staging::Include(paths)
                } else {
                    Staging::Only(paths)
                }
            } else if has("--all") || short_flag('a') {
                Staging::All
            } else {
                Staging::Index
            };
            Trigger::Commit {
                amend: has("--amend"),
                staging,
                steps: Vec::new(),
            }
        }
        (Event::PostToolUse, "commit") => {
            if has("--dry-run") {
                return None;
            }
            Trigger::Committed {
                amend: has("--amend"),
                onto: None,
            }
        }
        (Event::PreToolUse, "merge" | "cherry-pick") => {
            if ["--abort", "--continue", "--skip", "--quit"]
                .iter()
                .any(|f| has(f))
            {
                return None;
            }
            // Flags the commit verbs read a value after but these do not:
            // `-S` signs with an attached key id, `--squash` is a switch.
            let switches: Vec<String> = args
                .iter()
                .filter(|a| !matches!(a.as_str(), "-S" | "--squash" | "--gpg-sign"))
                .cloned()
                .collect();
            let targets: Vec<String> = positionals_skipping(&switches, MERGE_VALUE_FLAGS);
            if verb == "merge" {
                Trigger::Merge(targets.into_iter().next()?)
            } else if targets.is_empty() {
                return None;
            } else {
                Trigger::CherryPick(targets)
            }
        }
        (Event::PreToolUse, "push") => {
            if ["--tags", "--delete", "--dry-run"].iter().any(|f| has(f))
                || short_flag('d')
                || short_flag('n')
            {
                return None;
            }
            Trigger::Push(pushed(args)?)
        }
        (Event::PreToolUse, "gh pr") => {
            if args.first().map(String::as_str) != Some("create") {
                return None;
            }
            Trigger::PullRequest {
                base: flag_value(args, "--base", "-B"),
                // `owner:branch` is a fork's branch.
                head: flag_value(args, "--head", "-H").map(|h| match h.split_once(':') {
                    Some((_, branch)) => branch.to_string(),
                    None => h,
                }),
            }
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

fn same_dir(a: &Path, b: &Path) -> bool {
    match (dunce::canonicalize(a), dunce::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn first_reachable(root: &Path, revs: &[&str]) -> Option<String> {
    revs.iter()
        .find(|r| git_out(root, &["rev-parse", "--verify", "--quiet", r]).is_some())
        .map(|r| r.to_string())
}

/// The index captured as a tree against `base`, or against the snapshot's
/// own base (`HEAD`, the empty tree on an unborn branch). Captured from a
/// copy: `write-tree` on the real index takes its lock from under a
/// concurrent `git add`.
fn staged_range(root: &Path, base: Option<&str>) -> Option<String> {
    let staged = crate::git::capture_planned(root, crate::git::PlanStart::Index, &[]).ok()?;
    Some(format!("{}..{}", base.unwrap_or(&staged.base), staged.tree))
}

/// Where the command line's statements run: the payload's `cwd`, and the
/// directory a `cd` or `-C` named relative to it.
fn resolve_dir(cwd: &Path, dir: Option<&str>) -> PathBuf {
    match dir {
        Some(dir) if is_absolute_dir(dir) => PathBuf::from(dir),
        Some(dir) => cwd.join(dir),
        None => cwd.to_path_buf(),
    }
}

/// The range whose impact the tool call is about to make permanent. `cwd`
/// is where the agent ran the line and `here` where the triggering
/// statement runs, for the paths the line spells.
pub fn range_for(root: &Path, cwd: &Path, here: &Path, trigger: &Trigger) -> Option<String> {
    let parent = || git_out(root, &["rev-parse", "--verify", "--quiet", "HEAD~1"]);
    Some(match trigger {
        Trigger::Inspect { staged: false } => "HEAD".to_string(),
        Trigger::Inspect { staged: true } => staged_range(root, None)?,
        Trigger::Commit {
            amend,
            staging: Staging::Worktree,
            ..
        } => {
            if *amend {
                parent()?
            } else {
                "HEAD".to_string()
            }
        }
        Trigger::Commit {
            amend,
            staging,
            steps,
        } => {
            let base = if *amend { Some(parent()?) } else { None };
            if !trigger.is_preview() {
                return staged_range(root, base.as_deref());
            }
            planned_range(root, cwd, here, staging, steps, base.as_deref())?
        }
        Trigger::Committed { onto, .. } => format!("{}..HEAD", onto.as_deref()?),
        Trigger::Merge(target) => {
            // Catching up with the branch's own upstream brings in commits
            // that already landed there: nothing for this session to review.
            let tip = |rev: &str| git_out(root, &["rev-parse", "--verify", "--quiet", rev]);
            if tip(&format!("{target}^{{commit}}")).is_some_and(|t| tip("@{upstream}") == Some(t)) {
                return None;
            }
            let base = git_out(root, &["merge-base", "HEAD", target])?;
            format!("{base}..{target}")
        }
        Trigger::CherryPick(targets) => picked_range(root, targets)?,
        Trigger::Push(Pushed::Upstream) => {
            if first_reachable(root, &["@{upstream}"]).is_some() {
                "@{upstream}..HEAD".to_string()
            } else {
                // Without one, git sends the branch to `origin` by its name.
                push_range(root, "origin", "HEAD", None)?
            }
        }
        Trigger::Push(Pushed::Ref { remote, src, dst }) => {
            push_range(root, remote, src, dst.as_deref())?
        }
        Trigger::Push(Pushed::Unresolved) => return None,
        Trigger::PullRequest { base, head } => {
            // `--head` opens the PR from the branch as pushed.
            let head = match head {
                Some(h) => {
                    first_reachable(root, &[format!("refs/remotes/origin/{h}").as_str(), h])?
                }
                None => "HEAD".to_string(),
            };
            let base = match base {
                Some(b) => format!("refs/remotes/origin/{b}"),
                None => first_reachable(root, &["origin/HEAD", "origin/main", "origin/master"])?,
            };
            let merge_base = git_out(root, &["merge-base", &base, &head])?;
            format!("{merge_base}..{head}")
        }
    })
}

/// What a push of `src` sends to `remote`'s `dst` (`src`'s own branch when
/// the line names none): the commits past the remote's copy of that branch,
/// or past where `src` left the remote's default branch when it has none.
/// `None` for a ref git would refuse to push, or a remote it does not know.
fn push_range(root: &Path, remote: &str, src: &str, dst: Option<&str>) -> Option<String> {
    let exists = |rev: &str| git_out(root, &["rev-parse", "--verify", "--quiet", rev]).is_some();
    let dst = match dst {
        Some(dst) => dst.to_string(),
        None => git_out(root, &["rev-parse", "--symbolic-full-name", src])
            .filter(|full| full.starts_with("refs/"))?,
    };
    if !exists(&format!("{src}^{{commit}}")) {
        return None;
    }
    let tracking = |branch: &str| format!("refs/remotes/{remote}/{branch}");
    let copy = dst
        .strip_prefix("refs/heads/")
        .or_else(|| (!dst.starts_with("refs/")).then_some(dst.as_str()))
        .map(tracking)
        .filter(|copy| exists(copy));
    let base = match copy {
        Some(copy) => copy,
        None => {
            let default = ["HEAD", "main", "master"]
                .into_iter()
                .map(tracking)
                .find(|r| exists(r))?;
            git_out(root, &["merge-base", &default, src])?
        }
    };
    Some(format!("{base}..{src}"))
}

/// The tree `git commit` will start from once the line's own staging ran,
/// captured on a copy of the index: `HEAD..<tree>`, or `HEAD~1..<tree>` for
/// an amend. Pre-commit hooks may still rewrite it; the landed commit is
/// checked after.
fn planned_range(
    root: &Path,
    cwd: &Path,
    here: &Path,
    staging: &Staging,
    steps: &[Replay],
    base: Option<&str>,
) -> Option<String> {
    // A step run in another repository stages nothing this commit takes.
    let top = |dir: &Path| git_out(dir, &["rev-parse", "--show-toplevel"]).map(PathBuf::from);
    let mut plan: Vec<crate::git::PlanStep> = steps
        .iter()
        .map(|s| (resolve_dir(cwd, s.dir.as_deref()), s.args.clone()))
        .filter(|(dir, _)| dir.as_path() == here || top(dir).is_some_and(|t| same_dir(&t, root)))
        .collect();
    let own = |flags: &[&str], paths: &[String]| {
        let args = flags
            .iter()
            .map(|f| f.to_string())
            .chain(paths.iter().cloned())
            .collect();
        (here.to_path_buf(), args)
    };
    let start = match staging {
        Staging::All => {
            plan.push(own(&["add", "-u"], &[]));
            crate::git::PlanStart::Index
        }
        // A pathspec commit takes the paths the index already knows, never
        // an untracked file under them.
        Staging::Include(paths) => {
            plan.push(own(&["add", "-u", "--"], paths));
            crate::git::PlanStart::Index
        }
        // `--only` then drops whatever else was staged back to `HEAD`.
        Staging::Only(paths) => {
            plan.push(own(&["add", "-u", "--"], paths));
            let others: Vec<String> = std::iter::once(":/".to_string())
                .chain(paths.iter().map(|p| format!(":(exclude){p}")))
                .collect();
            plan.push(own(&["reset", "-q", "--"], &others));
            crate::git::PlanStart::Index
        }
        Staging::Index | Staging::Worktree => crate::git::PlanStart::Index,
    };
    let snapshot = crate::git::capture_planned(root, start, &plan).ok()?;
    Some(format!(
        "{}..{}",
        base.unwrap_or(&snapshot.base),
        snapshot.tree
    ))
}

/// What a cherry-pick lands: one commit or one range is its own diff; several
/// are applied in order onto a copy of `HEAD`, so a picked chain with a gap
/// is reviewed without the commits it skips (#380).
fn picked_range(root: &Path, targets: &[String]) -> Option<String> {
    if let [one] = targets {
        return Some(if one.contains("..") {
            one.clone()
        } else {
            format!("{one}^..{one}")
        });
    }
    let mut commits = Vec::new();
    for target in targets {
        if target.contains("..") {
            let listed = git_out(root, &["rev-list", "--reverse", target])?;
            commits.extend(listed.lines().map(str::to_string));
        } else {
            let spec = format!("{target}^{{commit}}");
            commits.push(git_out(root, &["rev-parse", "--verify", "--quiet", &spec])?);
        }
    }
    let snapshot =
        crate::git::capture_planned(root, crate::git::PlanStart::Picked(&commits), &[]).ok()?;
    Some(format!("{}..{}", snapshot.base, snapshot.tree))
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
    hash_working_tree_blobs(root, &mut entries);
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
/// with what `git add` would store. What git cannot hash — a nested
/// repository, a moved submodule, an unreadable file — keeps an identity of
/// its own: one such path used to fail the whole read, which every caller
/// logs as "clean", and the hook went quiet on a real change.
fn hash_working_tree_blobs(root: &Path, entries: &mut [(String, String, String)]) {
    let open = |oid: &str| oid.is_empty() || oid.bytes().all(|b| b == b'0');
    let files: Vec<String> = entries
        .iter()
        .filter(|(p, mode, oid)| open(oid) && mode != "160000" && root.join(p).is_file())
        .map(|(p, _, _)| p.clone())
        .collect();
    let hashed: FxHashMap<String, String> = crate::git::hash_object_paths(root, &files)
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
fn unhashable_identity(path: &Path) -> String {
    if path.is_dir() {
        if let Some(head) = git_out(path, &["rev-parse", "--verify", "--quiet", "HEAD"]) {
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
        // An empty answer is stored empty, as the hook's own path does: the
        // reminder then has no callers to point back at.
        let rendered = if output.empty {
            String::new()
        } else {
            crate::impact::render_markdown(output)
        };
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
        Trigger::Committed { .. } => "committed",
        Trigger::Merge(_) => "merge",
        Trigger::CherryPick(_) => "cherry-pick",
        Trigger::Push(_) => "push",
        Trigger::PullRequest { .. } => "pr",
        Trigger::Inspect { .. } => "inspect",
    }
}

fn payload_cwd(payload: &serde_json::Value) -> Option<PathBuf> {
    payload["cwd"]
        .as_str()
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
}

/// A relative directory on the command line is relative to where the agent
/// ran it, which the payload names; this process's cwd is a guess.
fn repo_root(payload: &serde_json::Value, dir: Option<&str>) -> Option<PathBuf> {
    let dir = resolve_dir(&payload_cwd(payload)?, dir);
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
    let mut detected = detect(event, command).ok_or_else(|| {
        if hides_git(command) {
            "unsupported-syntax"
        } else if statements(command).iter().any(|st| st.lost) {
            "unresolved-dir"
        } else {
            "no-trigger"
        }
    })?;
    trace.verb = Some(verb_of(&detected.trigger));
    let root = repo_root(payload, detected.dir.as_deref()).ok_or("not-git")?;
    trace.root = Some(root.clone());
    let head_before = head_record(&root, command, trace.session.as_deref());
    match &mut detected.trigger {
        Trigger::Committed { amend, onto } => {
            *onto = Some(committed_just_now(&root, &head_before, *amend)?);
        }
        _ if event == Event::PreToolUse => record_head(&root, &head_before),
        _ => {}
    }
    let cwd = payload_cwd(payload).ok_or("not-git")?;
    let here = resolve_dir(&cwd, detected.dir.as_deref());
    let unresolved = match detected.trigger {
        // A push or PR without a range names a ref nothing here resolves:
        // said so, never replaced by the current branch (#416).
        Trigger::Push(_) | Trigger::PullRequest { .. } => "unresolved-ref",
        _ => "no-range",
    };
    let range = range_for(&root, &cwd, &here, &detected.trigger).ok_or(unresolved)?;
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
/// says it did not try rather than guessing. A substitution elsewhere on the
/// line (`FP=$(cat f) && git commit`) hides nothing.
fn hides_git(command: &str) -> bool {
    const VERBS: [&str; 4] = ["git commit", "git push", "git merge", "git cherry-pick"];
    let spans = wrapped_spans(command);
    VERBS.iter().any(|verb| {
        command
            .match_indices(verb)
            .any(|(at, _)| spans.iter().any(|(from, to)| (*from..*to).contains(&at)))
    })
}

/// Byte ranges of `$(…)`, backtick pairs, `eval …` and `-c '…'` bodies.
fn wrapped_spans(command: &str) -> Vec<(usize, usize)> {
    let bytes = command.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"$(") {
            let mut depth = 0usize;
            let mut j = i + 1;
            while j < bytes.len() {
                match bytes[j] {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            spans.push((i, j));
        } else if bytes[i] == b'`' {
            let end = command[i + 1..]
                .find('`')
                .map_or(bytes.len(), |k| i + 1 + k);
            spans.push((i, end));
            i = end;
        } else if bytes[i..].starts_with(b"eval ") {
            let end = command[i..]
                .find(['\n', ';', '&', '|'])
                .map_or(bytes.len(), |k| i + k);
            spans.push((i, end));
        } else if bytes[i..].starts_with(b" -c '") || bytes[i..].starts_with(b" -c \"") {
            let quote = bytes[i + 4] as char;
            let end = command[i + 5..]
                .find(quote)
                .map_or(bytes.len(), |k| i + 5 + k);
            spans.push((i, end));
        }
        i += 1;
    }
    spans
}

/// Where a line's `PreToolUse` leaves `HEAD` as it found it, for the same
/// line's `PostToolUse`: one per session, repository and command.
fn head_record(root: &Path, command: &str, session: Option<&str>) -> String {
    let line = Fnv::new()
        .feed(root.to_string_lossy().as_bytes())
        .feed(command.as_bytes())
        .0;
    session_key(&format!("head-{line:016x}"), session)
}

/// `HEAD` before git runs; empty on an unborn branch. A `HEAD` git could
/// not be asked about leaves no record rather than a wrong one.
fn record_head(root: &Path, record: &str) {
    match crate::git::run_git(root, &["rev-parse", "--verify", "--quiet", "HEAD"]) {
        Ok(head) => mark_seen_with(record, head.trim()),
        Err(crate::git::GitError::CommandFailed(_)) => mark_seen_with(record, ""),
        Err(_) => unmark(record),
    }
}

/// What the line's own commit sits on: the `HEAD` its `PreToolUse` recorded,
/// once `HEAD` moved on from it, or for an amend that commit's parent. The
/// tool's result is not in the payload, so a `HEAD` still where the line
/// found it is a commit that failed, and one moved anywhere else is not
/// this line's: a commit another session just made used to pass for it
/// (#406).
fn committed_just_now(root: &Path, record: &str, amend: bool) -> Result<String, Silence> {
    let before = seen_headline(record, SEEN_TTL).ok_or("no-head-record")?;
    unmark(record);
    let head = crate::git::rev_oid(root, "HEAD").ok_or("no-new-commit")?;
    let descends = |base: &str| {
        crate::git::run_git(root, &["merge-base", "--is-ancestor", base, &head]).is_ok()
    };
    let parent = |rev: &str| {
        git_out(
            root,
            &["rev-parse", "--verify", "--quiet", &format!("{rev}~1")],
        )
    };
    if head == before {
        Err("no-new-commit")
    } else if before.is_empty() {
        // The branch was unborn: the line made its first commit.
        Ok(crate::git::empty_tree_oid(root))
    } else if descends(&before) {
        Ok(before)
    } else if amend {
        match parent(&before) {
            Some(base) if descends(&base) => Ok(base),
            // An amended first commit is a first commit again.
            None if parent(&head).is_none() => Ok(crate::git::empty_tree_oid(root)),
            _ => Err("foreign-head"),
        }
    } else {
        Err("foreign-head")
    }
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
        (
            Event::PreToolUse,
            Trigger::Commit {
                staging: Staging::Worktree,
                ..
            },
        ) => {
            "diffctx previewed what this command will record, estimated from the working tree before git runs; the commit is checked again once it lands."
        }
        (Event::PreToolUse, t) if t.is_preview() => {
            "diffctx previewed the index this commit will start from, with the staging on this line applied before git runs; pre-commit hooks may still change it, so the commit is checked again once it lands."
        }
        (Event::PreToolUse, _) => "diffctx reviewed the change this command is about to record.",
        (Event::PostToolUse, Trigger::Committed { .. }) => {
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
        .find(|st| st.verb == "grep" && !st.lost)?;
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
        // An answer that was empty stays empty: no reminder of callers that
        // were never listed, and the log says why the hook was silent.
        (Some(d), _) if !d.partial && d.headline.is_empty() => {
            if event == Event::PreToolUse {
                mark_seen(&prepared.record_key);
            }
            return Err("empty");
        }
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

    /// A commit with no staging on its line: `-a` or the index.
    fn commit(all: bool, amend: bool) -> Trigger {
        Trigger::Commit {
            amend,
            staging: if all { Staging::All } else { Staging::Index },
            steps: Vec::new(),
        }
    }

    fn only(paths: &[&str]) -> Trigger {
        Trigger::Commit {
            amend: false,
            staging: Staging::Only(paths.iter().map(|p| p.to_string()).collect()),
            steps: Vec::new(),
        }
    }

    fn after_adds(adds: &[&[&str]]) -> Trigger {
        Trigger::Commit {
            amend: false,
            staging: Staging::Index,
            steps: adds
                .iter()
                .map(|a| Replay {
                    dir: None,
                    args: a.iter().map(|w| w.to_string()).collect(),
                })
                .collect(),
        }
    }

    #[test]
    fn a_redirection_is_neither_a_pathspec_nor_an_operator() {
        for command in [
            "git commit -q -m x 2>&1 | tail -3",
            "git commit -m x >/dev/null 2>&1",
            "git commit -m x &> log.txt",
            "git commit -m x 2> err.txt; git log -1",
        ] {
            assert_eq!(pre(command), Some(commit(false, false)), "{command}");
        }
        assert_eq!(
            pre("git commit -m x src/a.py 2>&1"),
            Some(only(&["src/a.py"]))
        );
    }

    #[test]
    fn verbs_and_their_escapes() {
        assert_eq!(pre("git commit -m 'x'"), Some(commit(false, false)));
        assert_eq!(pre("git commit -am x"), Some(commit(true, false)));
        assert_eq!(pre("git commit --all -m x"), Some(commit(true, false)));
        assert_eq!(
            pre("git commit -m x -- src/a.py"),
            Some(only(&["src/a.py"]))
        );
        assert_eq!(
            detect(Event::PreToolUse, "cd repo && git -C /tmp/r commit -am x").unwrap(),
            Detected {
                trigger: commit(true, false),
                dir: Some("/tmp/r".to_string())
            }
        );
        assert_eq!(
            pre("git commit -q --amend --no-edit"),
            Some(commit(false, true))
        );
        assert!(pre("git commit --dry-run").is_none());
        assert_eq!(
            pre("git merge feature/x"),
            Some(Trigger::Merge("feature/x".to_string()))
        );
        assert!(pre("git merge --abort").is_none());
        assert_eq!(
            pre("git cherry-pick abc123"),
            Some(Trigger::CherryPick(vec!["abc123".to_string()]))
        );
        assert_eq!(
            pre("git cherry-pick -x -m 1 a b c"),
            Some(Trigger::CherryPick(vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string()
            ]))
        );
        assert_eq!(
            pre("git merge -m 'merge it' -X theirs feature/x"),
            Some(Trigger::Merge("feature/x".to_string()))
        );
        assert!(pre("git cherry-pick --continue").is_none());
        assert_eq!(
            pre("git push origin main"),
            Some(Trigger::Push(Pushed::Ref {
                remote: "origin".to_string(),
                src: "main".to_string(),
                dst: None
            }))
        );
        assert!(pre("git push --tags").is_none());
        assert!(pre("git push --dry-run").is_none());
        assert_eq!(
            pre("gh pr create --fill"),
            Some(Trigger::PullRequest {
                base: None,
                head: None
            })
        );
        assert!(pre("gh pr view 12").is_none());
        assert!(pre("git status && git log").is_none());
        // The add has not run when the hook sees the line: it is replayed
        // on a copy of the index before the commit is reviewed.
        assert_eq!(
            pre("git add -A && git commit -m x && git push"),
            Some(after_adds(&[&["add", "-A"]]))
        );
        assert_eq!(
            pre("git add a.py && git rm -r old && git commit -m x"),
            Some(after_adds(&[
                &["add", "a.py"],
                &["rm", "--cached", "-q", "-r", "old"]
            ]))
        );
        assert!(matches!(
            pre("git mv a b && git commit -m x"),
            Some(Trigger::Commit {
                staging: Staging::Worktree,
                ..
            })
        ));
        assert!(matches!(
            pre("git add -p && git commit -m x"),
            Some(Trigger::Commit {
                staging: Staging::Worktree,
                ..
            })
        ));
        assert_eq!(pre("git commit -qam x"), Some(commit(true, false)));
        assert_eq!(pre("git commit -S -m x"), Some(commit(false, false)));
        assert_eq!(
            pre("git commit -m x -i a.py"),
            Some(Trigger::Commit {
                amend: false,
                staging: Staging::Include(vec!["a.py".to_string()]),
                steps: Vec::new(),
            })
        );
        assert_eq!(pre("git commit -m a -m b"), Some(commit(false, false)));
        assert_eq!(pre("git commit -F msg.txt"), Some(commit(false, false)));
        assert_eq!(pre("git commit -m x src/a.py"), Some(only(&["src/a.py"])));
        assert_eq!(
            detect(Event::PreToolUse, "cd sub/app && git commit -m x").unwrap(),
            Detected {
                trigger: commit(false, false),
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
        assert!(pre("cd ~other/x && git commit -m x").is_none());
        let home = std::env::var("HOME").unwrap();
        assert_eq!(
            detect(Event::PreToolUse, "cd ~/x && git commit -m x")
                .unwrap()
                .dir,
            Some(format!("{}/x", home.trim_end_matches('/')))
        );
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
        let committed = Some(Trigger::Committed {
            amend: false,
            onto: None,
        });
        assert_eq!(post("git commit -m x"), committed);
        assert!(pre("git diff").is_none());
        // A status beside a diff only summarises it: the diff decides.
        for line in [
            "git status && git diff --cached",
            "git status --short; git diff --staged",
        ] {
            assert_eq!(
                post(line),
                Some(Trigger::Inspect { staged: true }),
                "{line}"
            );
        }
        assert_eq!(post("git status && git commit -m x"), committed);
    }

    #[test]
    fn the_first_statement_wins_and_operators_end_it() {
        assert_eq!(
            pre("git add -A; git commit -m 'a; b' | cat"),
            Some(after_adds(&[&["add", "-A"]]))
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
    fn a_heredoc_body_is_text_and_a_newline_ends_the_statement() {
        assert_eq!(
            pre("git commit -q -F - <<'EOF'\nfix: don't x; y | z\n\nbody `a` & b\nEOF"),
            Some(commit(false, false))
        );
        assert_eq!(
            pre("git commit -F- <<-\"EOF\" 2>&1 | tail -1\n\tmsg a.py\n\tEOF\ngit push"),
            Some(commit(false, false))
        );
        assert_eq!(
            pre("git add a.py\ngit commit -m x"),
            Some(after_adds(&[&["add", "a.py"]]))
        );
        assert_eq!(pre("git commit \\\n  -m x"), Some(commit(false, false)));
        assert_eq!(pre("cat <<< 'git commit -m x'"), None);
    }

    #[test]
    fn a_diff_written_to_a_file_is_no_inspection() {
        assert!(post("git diff > /tmp/all.patch").is_none());
        assert!(post("git diff >/dev/null").is_none());
        assert!(post("git diff &> out.txt").is_none());
        assert!(post("git diff 2>/dev/null").is_some());
        assert!(post("git diff >&2").is_some());
        assert!(pre("git commit -m x > log.txt").is_some());
    }

    #[test]
    fn only_git_inside_a_substitution_is_hidden() {
        assert!(hides_git("sh -c 'git commit -am x'"));
        assert!(hides_git("x=$(git commit -m y)"));
        assert!(hides_git("eval git push"));
        assert!(!hides_git("FP=$(cat f) && git commit -m x"));
        assert!(!hides_git("git commit -m 'a `b`' && echo $(date)"));
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
