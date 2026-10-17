//! `diffctx hook <event>`: the Claude Code hooks that put a change's impact
//! in front of the agent without asking it to call anything (#310).
//!
//! * `pretooluse`: before a line that commits, merges, picks, reverts or
//!   pushes, the refs of each repository it names, and nothing else; before
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
    /// counts the other refs one push moved, which are not reviewed.
    Landed { verb: &'static str, also: usize },
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

/// The changed files an inspecting session edited itself, in a checkout
/// other sessions may share.
#[derive(Debug, PartialEq, Eq)]
pub struct Owned {
    pub files: Vec<String>,
    /// Changed files no edit of this session touched: counted, not reviewed.
    pub others: usize,
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
            // Inside `"…"` a backslash escapes `"`, `\`, `$` and a backtick
            // and joins lines; before anything else it is itself (#436).
            Some('"') if c == '\\' => match chars.next_if(|n| "\"\\$`\n".contains(*n)) {
                Some('\n') => {}
                Some(n) => cur.push(n),
                None => cur.push(c),
            },
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
    // `L=/abs/path; cd $L`: a variable the line itself sets to a literal
    // directory names that directory (#422).
    let mut vars: FxHashMap<String, String> = FxHashMap::default();
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
        let head = i == 0 || is_operator(&ws[i - 1]) || ws[i - 1] == "export";
        if let Some((name, value)) = ws[i].split_once('=').filter(|_| head) {
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                let value = expand_home(&substitute(value, &vars));
                if is_unresolvable(&value) {
                    vars.remove(name);
                } else {
                    vars.insert(name.to_string(), value);
                }
                i += 1;
                continue;
            }
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
            if let Some(target) = target.map(|w| expand_home(&substitute(w, &vars))) {
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
                    if let Some(d) = ws.get(j + 1).map(|d| expand_home(&substitute(d, &vars))) {
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
                if prog == "git"
                    && matches!(
                        w.as_str(),
                        "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env"
                    )
                {
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
                    } else if verb == "grep" {
                        // Its positionals are revisions, not paths (#486).
                        "git grep".to_string()
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
/// `git grep PATTERN [REV…] [-- PATH…]` names its paths only after `--`;
/// a word between the pattern and `--` is a revision (`origin/main`).
fn search_pattern(args: &[String], git: bool) -> Option<(String, Option<String>)> {
    let mut pattern = None;
    let mut paths_follow = !git;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if git && a == "--" {
            paths_follow = true;
        } else if a == "-e" || a == "--regexp" {
            pattern = it.next().cloned();
        } else if let Some(p) = a.strip_prefix("--regexp=") {
            pattern = Some(p.to_string());
        } else if SEARCH_VALUE_FLAGS.split(' ').any(|f| f == a) {
            it.next();
        } else if a.starts_with('-') {
            // A boolean flag, or `--` before the paths.
        } else if pattern.is_none() {
            pattern = Some(a.clone());
        } else if paths_follow {
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
/// `\(`, `\w`) and declaration keywords dropped: one per alternation branch
/// that names exactly one, read off the end of its qualified chain —
/// `tests.direct`, `tests\.direct` and `a::direct` search for `direct`, not
/// for `tests` (#402). A test module or test function is no definition a
/// caller list answers for.
fn pattern_names(pattern: &str) -> Vec<String> {
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
        let branch_names: Vec<&str> = cleaned
            .split_whitespace()
            .filter_map(|chain| chain.split('.').filter(|s| !s.is_empty()).next_back())
            .filter(|w| {
                w.chars().count() >= 2
                    && !w.starts_with(|c: char| c.is_ascii_digit())
                    && !DECLARATION_WORDS.split(' ').any(|d| d == *w)
            })
            .collect();
        // A branch that names several identifiers is a line of code, not a
        // search for one of them (`for (path, line, text) in hits`).
        if let [w] = branch_names[..] {
            if !matches!(w, "test" | "tests")
                && !w.starts_with("test_")
                && !names.iter().any(|n| n == w)
            {
                names.push(w.to_string());
            }
        }
    }
    names.truncate(8);
    names
}

/// A directory only the shell could expand: a variable, a command
/// substitution, another user's home (`~user`).
/// `$NAME` and `${NAME}` replaced by what the line set them to; any other
/// expansion is left for `is_unresolvable` to see.
fn substitute(word: &str, vars: &FxHashMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = word;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let (name, len) = match after.strip_prefix('{') {
            Some(braced) => braced
                .find('}')
                .map_or(("", 0), |end| (&braced[..end], end + 2)),
            None => {
                let end = after
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(after.len());
                (&after[..end], end)
            }
        };
        match vars.get(name) {
            Some(value) => {
                out.push_str(value);
                rest = &after[len..];
            }
            None => {
                out.push('$');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

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
    // A line that stages before it commits is not a plain commit: the gate
    // asks for the staging to run first instead of predicting it.
    let mut stages = false;
    let mut summary = None;
    for st in statements(command).into_iter().filter(|st| !st.lost) {
        if STAGING_VERBS.contains(&st.verb.as_str()) {
            stages = true;
        }
        if let Some(mut detected) = detect_statement(event, &st) {
            // `git status` only summarises: a diff later on the same line is
            // what the agent read, the index alone for `--cached`.
            if st.verb == "status" {
                summary.get_or_insert(detected);
                continue;
            }
            if let Trigger::Commit { plain, .. } = &mut detected.trigger {
                *plain &= !stages;
            }
            return Some(detected);
        }
    }
    summary
}

/// Verbs that write the index or the files a commit reads.
const STAGING_VERBS: [&str; 20] = [
    "add",
    "stage",
    "rm",
    "mv",
    "reset",
    "restore",
    "checkout",
    "stash",
    "apply",
    "am",
    "update-index",
    "read-tree",
    "checkout-index",
    "switch",
    "merge",
    "cherry-pick",
    "revert",
    "pull",
    "rebase",
    "clean",
];

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
            let plain = pathspecs(&unsigned).is_empty()
                && !has("--all")
                && !short_flag('a')
                && !has("--include")
                && !short_flag('i')
                && !has("--only")
                && !short_flag('o')
                && !args.iter().any(|a| a.starts_with("--pathspec-from-file"));
            Trigger::Commit {
                amend: has("--amend"),
                plain,
            }
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
        (Event::PostToolUse { .. }, "diff") => {
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
                own: None,
            }
        }
        (Event::PostToolUse { .. }, "status") if !st.piped_out => Trigger::Inspect {
            staged: false,
            own: None,
        },
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

/// The index captured as a tree against `base`, or against the snapshot's
/// own base (`HEAD`, the empty tree on an unborn branch). Captured from a
/// copy: `write-tree` on the real index takes its lock from under a
/// concurrent `git add`.
fn staged_range(root: &Path, base: Option<&str>) -> Option<String> {
    let staged = crate::git::capture_planned(root, &[]).ok()?;
    Some(format!("{}..{}", base.unwrap_or(&staged.base), staged.tree))
}

/// This session's files as they stand (as staged, for `--cached`) and every
/// other path as `HEAD` has it: the pending change without the edits other
/// sessions left in the same checkout, captured on a copy of the index.
fn own_range(root: &Path, staged: bool, files: &[String]) -> Option<String> {
    let step = |args: &[&str], magic: &str| -> crate::git::PlanStep {
        let paths = files.iter().map(|f| format!(":({magic}){f}"));
        let args = args.iter().map(|a| a.to_string()).chain(paths).collect();
        (root.to_path_buf(), args)
    };
    let mut plan = Vec::new();
    if !staged {
        plan.push(step(&["add", "-A", "--"], "literal"));
    }
    plan.push(step(&["reset", "-q", "--", ":/"], "exclude,literal"));
    let snapshot = crate::git::capture_planned(root, &plan).ok()?;
    Some(format!("{}..{}", snapshot.base, snapshot.tree))
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

/// The range a trigger read before git runs is about. `cwd` is where the
/// agent ran the line; a landed record carries its own range.
pub fn range_for(root: &Path, trigger: &Trigger) -> Option<String> {
    Some(match trigger {
        Trigger::Inspect {
            staged,
            own: Some(own),
        } if own.others > 0 => own_range(root, *staged, &own.files)?,
        Trigger::Inspect { staged: false, .. } => "HEAD".to_string(),
        Trigger::Inspect { staged: true, .. } => staged_range(root, None)?,
        Trigger::Commit { amend, .. } => {
            let base = if *amend {
                Some(git_out(
                    root,
                    &["rev-parse", "--verify", "--quiet", "HEAD~1"],
                )?)
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
        Trigger::Landed { .. } => return None,
    })
}

/// What a line recorded in each repository it may have touched, as its
/// `PreToolUse` found them: `HEAD` (empty on an unborn branch) and every
/// remote-tracking ref.
#[derive(serde::Serialize, serde::Deserialize)]
struct Before {
    root: PathBuf,
    head: String,
    remotes: Vec<(String, String)>,
}

fn refs_now(root: &Path) -> Option<Before> {
    let head = match crate::git::run_git(root, &["rev-parse", "--verify", "--quiet", "HEAD"]) {
        Ok(head) => head.trim().to_string(),
        Err(crate::git::GitError::CommandFailed(_)) => String::new(),
        Err(_) => return None,
    };
    let remotes = git_out(
        root,
        &[
            "for-each-ref",
            // `origin/HEAD` names another ref: not a ref a push moves.
            "--format=%(if)%(symref)%(then)%(else)%(refname) %(objectname)%(end)",
            "refs/remotes",
        ],
    )
    .unwrap_or_default()
    .lines()
    .filter_map(|l| l.split_once(' '))
    .map(|(r, o)| (r.to_string(), o.to_string()))
    .collect();
    Some(Before {
        root: root.to_path_buf(),
        head,
        remotes,
    })
}

/// The record one line leaves for its own `PostToolUse`: per session and
/// command, so another session's commit is never this line's.
fn before_key(command: &str, session: Option<&str>) -> String {
    let line = Fnv::new().feed(command.as_bytes()).0;
    session_key(&format!("before-{line:016x}"), session)
}

/// What the line recorded in `before.root`, by git's own reflog: a push
/// (what a remote-tracking ref moved over), else a commit, merge,
/// cherry-pick or revert on `HEAD`. A checkout, reset, pull or rebase moved
/// `HEAD` without recording this session's change, and is not reviewed.
/// The count is of the other refs the same push moved.
fn landed(before: &Before) -> Option<(String, &'static str, usize)> {
    let root = before.root.as_path();
    let now = refs_now(root)?;
    let old_of = |name: &str| {
        before
            .remotes
            .iter()
            .find(|(r, _)| r == name)
            .map(|(_, o)| o.clone())
    };
    let pushed: Vec<&(String, String)> = now
        .remotes
        .iter()
        .filter(|(name, new)| {
            old_of(name).as_deref() != Some(new.as_str())
                && git_out(root, &["reflog", "-1", "--format=%gs", name])
                    .is_some_and(|s| s.starts_with("update by push"))
        })
        .collect();
    if let Some((name, new)) = pushed.first() {
        // What the push itself moved the ref over: a `pull` on the same line
        // moves it first, and the value this line started from would count
        // the commits that pull brought in as pushed (#462).
        let moved_over = git_out(
            root,
            &["rev-parse", "--verify", "--quiet", &format!("{name}@{{1}}")],
        );
        let base = match moved_over.or_else(|| old_of(name)) {
            Some(old) => old,
            // A new branch: the commits past where it left the remote's default.
            None => {
                let remote = name.strip_prefix("refs/remotes/")?.split('/').next()?;
                let default = ["HEAD", "main", "master"]
                    .iter()
                    .map(|b| format!("refs/remotes/{remote}/{b}"))
                    .find(|r| r != name && crate::git::rev_oid(root, r).is_some())?;
                git_out(root, &["merge-base", &default, new])?
            }
        };
        return Some((format!("{base}..{new}"), "push", pushed.len() - 1));
    }
    if now.head == before.head || now.head.is_empty() {
        return None;
    }
    let head = now.head.as_str();
    let subject = git_out(root, &["reflog", "-1", "--format=%gs", "HEAD"])?;
    let action = subject.split(':').next().unwrap_or_default();
    let empty = || crate::git::empty_tree_oid(root);
    let verb = match action {
        "commit (initial)" => return Some((format!("{}..{head}", empty()), "commit", 0)),
        "commit (amend)" => {
            let parent = git_out(root, &["rev-parse", "--verify", "--quiet", "HEAD~1"]);
            return Some((
                format!("{}..{head}", parent.unwrap_or_else(empty)),
                "commit",
                0,
            ));
        }
        "commit" | "commit (merge)" => "commit",
        "cherry-pick" => "cherry-pick",
        "revert" => "revert",
        // Catching up with the branch's own upstream brings in commits that
        // already landed there: nothing for this session to review.
        a if a.starts_with("merge ") => {
            if subject.contains("Fast-forward")
                && crate::git::rev_oid(root, "@{upstream}").as_deref() == Some(head)
            {
                return None;
            }
            "merge"
        }
        _ => return None,
    };
    if before.head.is_empty() {
        return Some((format!("{}..{head}", empty()), verb, 0));
    }
    crate::git::run_git(root, &["merge-base", "--is-ancestor", &before.head, head])
        .is_ok()
        .then(|| (format!("{}..{head}", before.head), verb, 0))
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

/// Where the hook keeps what it delivered and its log. A cache it cannot
/// write disabled the dedupe and the retry cap without a word, and every
/// commit reran the pipeline (#407): the temporary directory stands in.
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
    if let Trigger::Inspect { staged, own } = &mut detected.trigger {
        *own = own_changes(&root, payload, *staged)?;
    }
    // A PR without a range names a ref nothing here resolves: said so,
    // never replaced by the current branch (#416).
    let unresolved = match detected.trigger {
        Trigger::PullRequest { .. } => "unresolved-ref",
        _ => "no-range",
    };
    let range = range_for(&root, &detected.trigger).ok_or(unresolved)?;
    prepared(root, range, detected.trigger, trace)
}

/// What the line recorded, from the refs its `PreToolUse` left: the first
/// repository it touched that moved.
fn prepare_landed(command: &str, trace: &mut Trace) -> Result<Prepared, Silence> {
    let key = before_key(command, trace.session.as_deref());
    let before: Vec<Before> = seen_headline(&key, SEEN_TTL)
        .and_then(|s| serde_json::from_str(&s).ok())
        .ok_or("no-record")?;
    unmark(&key);
    let (root, (range, verb, also)) = before
        .iter()
        .find_map(|b| Some((b.root.clone(), landed(b)?)))
        .ok_or("nothing-landed")?;
    trace.verb = Some(verb);
    trace.root = Some(root.clone());
    prepared(root, range, Trigger::Landed { verb, also }, trace)
}

fn prepared(
    root: PathBuf,
    range: String,
    trigger: Trigger,
    trace: &mut Trace,
) -> Result<Prepared, Silence> {
    trace.range = Some(range.clone());
    let identity = result_identity(&root, &range).ok_or("clean")?;
    let session = trace.session.as_deref();
    let key = session_key(&format!("id-{identity}"), session);
    let shown =
        delivered(&key, SEEN_TTL).or_else(|| delivered(&format!("id-{identity}"), MANUAL_SEEN_TTL));
    Ok(Prepared {
        root,
        range,
        trigger,
        record_key: session_key(&format!("rec-{identity}"), session),
        tries_key: session_key(&format!("try-{identity}"), session),
        key,
        shown,
    })
}

/// Before a line that records or publishes commits runs: the refs of every
/// repository it names (the cwd's, a `cd` or `-C` target), for its own
/// `PostToolUse` to compare against. Nothing about what it will stage is
/// guessed. A directory only the shell can name, or a git the shell alone
/// runs, is said to be unread.
fn record_before(
    payload: &serde_json::Value,
    command: &str,
    trace: &mut Trace,
) -> Result<(), Silence> {
    if hides_git(command) {
        return Err("unsupported-syntax");
    }
    let cwd = payload_cwd(payload).ok_or("not-git")?;
    // The cwd's own repository always: a statement this parser misreads
    // still records there.
    let mut roots: Vec<PathBuf> = git_out(&cwd, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .into_iter()
        .collect();
    for st in statements(command)
        .into_iter()
        .filter(|st| RECORDING_VERBS.contains(&st.verb.as_str()))
    {
        if st.lost {
            return Err("unresolved-dir");
        }
        let top = git_out(
            &resolve_dir(&cwd, st.dir.as_deref()),
            &["rev-parse", "--show-toplevel"],
        );
        if let Some(top) = top.map(PathBuf::from).filter(|t| !roots.contains(t)) {
            roots.push(top);
        }
    }
    let before: Vec<Before> = roots.iter().filter_map(|r| refs_now(r)).collect();
    if before.is_empty() {
        return Err("not-git");
    }
    trace.root = before.first().map(|b| b.root.clone());
    let json = serde_json::to_string(&before).map_err(|_| "error:record")?;
    mark_seen_with(&before_key(command, trace.session.as_deref()), &json);
    Ok(())
}

/// The verbs whose effect is a commit or a pushed ref.
const RECORDING_VERBS: [&str; 5] = ["commit", "merge", "cherry-pick", "revert", "push"];

/// A git command only the shell could run — in `$(…)`, backticks, `eval`,
/// `sh -c '…'`: what it records cannot be read off the line, and the hook
/// says it did not try rather than guessing. A substitution elsewhere on the
/// line (`FP=$(cat f) && git commit`) hides nothing.
fn hides_git(command: &str) -> bool {
    wrapped_spans(command)
        .iter()
        .any(|&(from, to)| command.get(from..to).is_some_and(names_a_recording_verb))
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
        } else if let Some(open) = shell_c_flag(&bytes[i..]) {
            // `-c`, and clusters ending in it (`-lc`, `-ic`): `bash -lc "…"`.
            let quote = bytes[i + open] as char;
            let end = command[i + open + 1..]
                .find(quote)
                .map_or(bytes.len(), |k| i + open + 1 + k);
            spans.push((i, end));
        }
        i += 1;
    }
    spans
}

/// The offset of the opening quote after a ` -c` flag or a short-flag
/// cluster ending in `c` (` -lc '`), if `rest` starts with one.
fn shell_c_flag(rest: &[u8]) -> Option<usize> {
    let flags = rest.strip_prefix(b" -")?;
    let len = flags.iter().take_while(|b| b.is_ascii_alphabetic()).count();
    (len > 0
        && flags[len - 1] == b'c'
        && flags.get(len) == Some(&b' ')
        && matches!(flags.get(len + 1), Some(b'\'' | b'"')))
    .then_some(2 + len + 1)
}

/// Bytes of transcript read to tell this session's edits apart. A long
/// session writes tens of megabytes; past this the answer says it cannot
/// tell rather than miss an edit made early on.
const TRANSCRIPT_BUDGET: u64 = 256 * 1024 * 1024;

const EDIT_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// Which of the changed files an inspection shows this session edited, by
/// its own transcript; `None` when no transcript can tell. Another session's
/// uncommitted work in a shared checkout is not this one's pending change:
/// a session that edited none of the changed files has nothing to review
/// (#404).
fn own_changes(
    root: &Path,
    payload: &serde_json::Value,
    staged: bool,
) -> Result<Option<Owned>, Silence> {
    let changed = changed_paths(root, staged);
    if changed.is_empty() {
        return Err("clean");
    }
    let Some(edited) = payload["transcript_path"]
        .as_str()
        .and_then(|t| edited_files(Path::new(t)))
    else {
        return Ok(None);
    };
    let top = canonical(root);
    let (files, others): (Vec<String>, Vec<String>) = changed
        .into_iter()
        .partition(|p| edited.contains(&top.join(p)));
    if files.is_empty() {
        return Err("not-this-session");
    }
    Ok(Some(Owned {
        files,
        others: others.len(),
    }))
}

/// Every path the inspected snapshot changes: the index against `HEAD`, or
/// the working tree with its untracked files.
fn changed_paths(root: &Path, staged: bool) -> Vec<String> {
    let args: &[&str] = if staged {
        &["diff", "--cached", "--name-only", "-z", "--no-renames"]
    } else {
        &["diff", "--name-only", "-z", "--no-renames", "HEAD", "--"]
    };
    let mut paths = crate::git::run_git_z(root, args).unwrap_or_default();
    if !staged {
        let untracked = ["ls-files", "-o", "--exclude-standard", "-z"];
        paths.extend(crate::git::run_git_z(root, &untracked).unwrap_or_default());
    }
    paths
}

/// The files this session's edit tools named, read from its transcript and
/// from its subagents' beside it (`<transcript>/subagents/`), streamed line
/// by line. `None` when they cannot be read whole within the budget.
fn edited_files(transcript: &Path) -> Option<rustc_hash::FxHashSet<PathBuf>> {
    use std::io::{BufRead as _, Read as _};
    let mut sources = vec![transcript.to_path_buf()];
    if let Ok(dir) = std::fs::read_dir(transcript.with_extension("").join("subagents")) {
        sources.extend(
            dir.filter_map(|e| Some(e.ok()?.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl")),
        );
    }
    let mut left = TRANSCRIPT_BUDGET;
    let mut edited = rustc_hash::FxHashSet::default();
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
            if text.contains("\"tool_use\"")
                && EDIT_TOOLS
                    .iter()
                    .any(|tool| text.contains(&format!("\"{tool}\"")))
            {
                edited.extend(edited_paths(text));
            }
        }
    }
    Some(edited)
}

/// The paths a transcript line's edit tool calls name.
fn edited_paths(line: &str) -> Vec<PathBuf> {
    let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
        return Vec::new();
    };
    entry["message"]["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|call| {
            call["type"] == "tool_use"
                && call["name"]
                    .as_str()
                    .is_some_and(|name| EDIT_TOOLS.contains(&name))
        })
        .filter_map(|call| {
            let input = &call["input"];
            input["file_path"]
                .as_str()
                .or_else(|| input["notebook_path"].as_str())
        })
        .map(|path| canonical(Path::new(path)))
        .collect()
}

/// A path as the filesystem resolves it (`/var` is `/private/var` on
/// macOS), through its directory when the file itself is gone.
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

fn lead_of(event: Event, trigger: &Trigger) -> String {
    let lead = match (event, trigger) {
        (_, Trigger::Commit { .. }) => "diffctx reviewed the index this commit will record.",
        (_, Trigger::Landed { verb, also: 0 }) => {
            return format!("diffctx reviewed what this {verb} recorded.");
        }
        (_, Trigger::Landed { verb, also }) => {
            return format!(
                "diffctx reviewed the first ref this {verb} moved; {also} other pushed ref(s) are not analysed."
            );
        }
        (Event::PreToolUse, _) => "diffctx reviewed the commits this pull request proposes.",
        (Event::PostToolUse { .. }, Trigger::Inspect { own: None, .. }) => {
            "diffctx reviewed every pending change in this checkout, which you just inspected; without a readable transcript it cannot tell this session's edits from another session's."
        }
        (Event::PostToolUse { .. }, Trigger::Inspect { own: Some(own), .. }) if own.others > 0 => {
            return format!(
                "diffctx reviewed the pending change this session and its subagents made, which you just inspected; {} other changed file(s) were not edited in this session or by its subagents and are not analysed.",
                own.others
            );
        }
        (Event::PostToolUse { .. }, _) => "diffctx reviewed the pending change you just inspected.",
    };
    lead.to_string()
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
    // With no caller listed, what is left to check is a contract or an
    // untested definition, not a caller (#463).
    let check = if output.changed.iter().any(|c| !c.callers.is_empty()) {
        "Check these callers"
    } else {
        "Check these"
    };
    let text = crate::impact::render_automatic(&mut output, AUTOMATIC_TOKEN_CAP);
    let headline = text.lines().next().unwrap_or_default().to_string();
    Ok(Answer {
        context: format!(
            "{} What it reaches outside the diff:\n{text}\n{check} before going on, or tell the user why they are unaffected.",
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
    let Statement {
        dir, args, verb, ..
    } = statements(command)
        .into_iter()
        .find(|st| (st.verb == "grep" || st.verb == "git grep") && !st.lost)?;
    let (pattern, path) = search_pattern(&args, verb == "git grep")?;
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
    // The pending change as impact reads it, untracked files included: a
    // name defined only in a new file is still the change's (#448).
    let Ok((hunks, _)) =
        crate::pipeline::collect_hunks_with_untracked(root, Some("HEAD"), &[], true, None)
    else {
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
    let command = payload["tool_input"]["command"]
        .as_str()
        .unwrap_or_default();
    let recording = names_a_recording_verb(command);
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
        let gated_commit = gate
            && matches!(
                detect(event, command).map(|d| d.trigger),
                Some(Trigger::Commit { .. })
            );
        match recorded {
            Ok(()) if !gated_commit && !command.contains("gh pr create") => {
                return Err("recorded");
            }
            Ok(()) | Err("not-git") => {}
            Err(why) => return Ok(not_analysed(event, why, trace)),
        }
    }
    if gate && event == Event::PreToolUse {
        if let Some(Trigger::Commit { plain: false, .. }) =
            detect(event, command).map(|d| d.trigger)
        {
            trace.noted = Some("denied");
            return Ok(serde_json::json!({ "hookSpecificOutput": {
                "hookEventName": event.name(),
                "permissionDecision": "deny",
                "permissionDecisionReason": "diffctx's strict gate reviews the index a commit records, so it does not run a commit that stages for itself. Run the staging (`git add`, `git rm`, ...) as its own command, then `git commit` without paths, `-a`, `-i` or `--pathspec-from-file`.",
            }})
            .to_string());
        }
    }
    let prepared = match if matches!(event, Event::PostToolUse { .. }) && recording {
        match prepare_landed(command, trace) {
            Err("no-record" | "nothing-landed") => match &search {
                Some(search) => return answer_search(search, trace),
                None => prepare(event, &payload, trace),
            },
            landed => landed,
        }
    } else {
        prepare(event, &payload, trace)
    } {
        Ok(p) => p,
        // A commit or push the hook could not read is said so, so that no
        // block means the hook did not run, never "reviewed, nothing" (#464).
        Err(why @ ("unsupported-syntax" | "unresolved-dir"))
            if event == Event::PreToolUse && recording =>
        {
            return Ok(not_analysed(event, why, trace));
        }
        Err(why) => return Err(why),
    };
    if event == Event::PreToolUse && is_seen(&prepared.record_key) {
        return Err("seen");
    }
    let landed = matches!(prepared.trigger, Trigger::Landed { .. });
    match (&prepared.shown, event) {
        // An answer that was empty stays empty: no reminder of callers that
        // were never listed. A landed record still says it in one line.
        (Some(d), _) if !d.partial && d.headline.is_empty() => {
            if event == Event::PreToolUse {
                mark_seen(&prepared.record_key);
            }
            if landed {
                return Ok(nothing_reached(event, trace));
            }
            return Err("empty");
        }
        // What landed was reviewed before (the gate, an inspection, a manual
        // run): pointed at, never silence, which read as no review (#501).
        (Some(d), _) if !d.partial && (event == Event::PreToolUse || landed) => {
            mark_seen(&prepared.record_key);
            trace.noted = Some("reminder");
            return Ok(hook_json(event, &reminder(event, &d.headline), false));
        }
        (Some(d), Event::PostToolUse { .. }) if !d.partial => return Err("seen"),
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
    match answer {
        Err("empty") if event == Event::PreToolUse || landed => Ok(nothing_reached(event, trace)),
        answer => answer.map(|a| {
            let deny = gate && !a.partial;
            if deny {
                trace.noted = Some("denied");
            }
            hook_json(event, &a.context, deny)
        }),
    }
}

/// A line that records or publishes commits: the events whose silence would
/// otherwise read as a clean review.
/// One line, so that no block means the hook did not run (#464).
fn nothing_reached(event: Event, trace: &mut Trace) -> String {
    trace.noted = Some("empty");
    let range = trace.range.as_deref().unwrap_or("this change");
    hook_json(
        event,
        &format!("diffctx: {range} reaches nothing outside the diff."),
        false,
    )
}

fn not_analysed(event: Event, why: Silence, trace: &mut Trace) -> String {
    trace.noted = Some(why);
    let how = if why == "unresolved-dir" {
        "it changes into a directory the hook cannot resolve"
    } else {
        "its git runs inside a subshell, eval or command substitution"
    };
    hook_json(
        event,
        &format!(
            "diffctx did not analyse this command: {how}. Run `diffctx <repo> --diff --mode impact` in that repository before going on."
        ),
        false,
    )
}

fn names_a_recording_verb(command: &str) -> bool {
    let words = || command.split(|c: char| !(c.is_alphanumeric() || c == '-'));
    words().any(|w| w == "git")
        && words().any(|w| matches!(w, "commit" | "push" | "merge" | "cherry-pick" | "revert"))
}

/// The log is cut over to `hook.log.1` past this size: tens of thousands of
/// runs, so an audit reads a whole session (#464).
const LOG_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// One line per run in `<cache>/diffctx/hook.log` (#338): Claude Code keeps
/// only hooks that print, so without it a silent hook that decided "nothing
/// to say" and one that never got an answer look the same afterwards.
/// `<unix ts> <session> <event> <verb> <range> <outcome> <ms>ms <repo>
/// <version>`: the version last, so an audit can tell an old pin's answer
/// from the current one (#524) without moving the fields before it.
fn log_outcome(event: Event, trace: &Trace, outcome: &str, elapsed: Duration) {
    let Some(dir) = cache_root() else { return };
    let path = dir.join("hook.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_MAX_BYTES) {
        let _ = std::fs::rename(&path, dir.join("hook.log.1"));
    }
    let session = file_safe(trace.session.as_deref());
    let line = format!(
        "{} {} {} {} {} {outcome} {}ms {} {}\n",
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
        env!("CARGO_PKG_VERSION"),
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
    let outcome = match &answer {
        Ok(_) => trace.noted.unwrap_or("shown"),
        Err(why) => why,
    };
    log_outcome(event, &trace, outcome, started.elapsed());
    match (answer.ok(), upgrade_notice(trace.session.as_deref())) {
        (answer, None) => answer,
        (None, Some(notice)) => Some(hook_json(event, &notice, false)),
        (Some(answer), Some(notice)) => Some(with_notice(&answer, &notice)),
    }
}

/// How long a session is told about one newer version: once, for any
/// session that lasts less than this.
const UPGRADE_NOTICE_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// #524: a session runs the plugin it started with, and a version installed
/// since sits beside it in the plugin cache. Claude Code exports the plugin's
/// root to its hooks (`CLAUDE_PLUGIN_ROOT`), so the wrapper forwards nothing;
/// a newer `X.Y.Z` sibling is said once per session.
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

fn release_key(name: &str) -> Option<(u64, u64, u64)> {
    let mut parts = name.split('.').map(|p| p.parse::<u64>().ok());
    let key = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(key)
}

fn with_notice(answer: &str, notice: &str) -> String {
    let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(answer) else {
        return answer.to_string();
    };
    let context = &mut doc["hookSpecificOutput"]["additionalContext"];
    let rest = context.as_str().unwrap_or_default();
    *context = serde_json::Value::String(format!("{notice}\n\n{rest}"));
    doc.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pre(command: &str) -> Option<Trigger> {
        detect(Event::PreToolUse, command).map(|d| d.trigger)
    }

    fn post(command: &str) -> Option<Trigger> {
        detect(Event::PostToolUse { failed: false }, command).map(|d| d.trigger)
    }

    fn inspection(staged: bool) -> Trigger {
        Trigger::Inspect { staged, own: None }
    }

    /// A commit the gate can review from the index (`plain`), or one that
    /// stages on its own line, names paths or takes `-a`.
    fn commit(plain: bool, amend: bool) -> Trigger {
        Trigger::Commit { amend, plain }
    }

    #[test]
    fn a_redirection_is_neither_a_pathspec_nor_an_operator() {
        for command in [
            "git commit -q -m x 2>&1 | tail -3",
            "git commit -m x >/dev/null 2>&1",
            "git commit -m x &> log.txt",
            "git commit -m x 2> err.txt; git log -1",
        ] {
            assert_eq!(pre(command), Some(commit(true, false)), "{command}");
        }
        assert_eq!(
            pre("git commit -m x src/a.py 2>&1"),
            Some(commit(false, false))
        );
    }

    #[test]
    fn verbs_and_their_escapes() {
        assert_eq!(pre("git commit -m 'x'"), Some(commit(true, false)));
        assert_eq!(pre("git commit -am x"), Some(commit(false, false)));
        assert_eq!(pre("git commit --all -m x"), Some(commit(false, false)));
        assert_eq!(
            pre("git commit -m x -- src/a.py"),
            Some(commit(false, false))
        );
        assert_eq!(
            pre("git commit --pathspec-from-file=list.txt -m x"),
            Some(commit(false, false))
        );
        assert_eq!(
            detect(Event::PreToolUse, "cd repo && git -C /tmp/r commit -am x").unwrap(),
            Detected {
                trigger: commit(false, false),
                dir: Some("/tmp/r".to_string())
            }
        );
        assert_eq!(
            pre("git commit -q --amend --no-edit"),
            Some(commit(true, true))
        );
        assert!(pre("git commit --dry-run").is_none());
        // Merges, picks and pushes are read off git's record once they ran,
        // never predicted from the line.
        for line in [
            "git merge feature/x",
            "git cherry-pick abc123",
            "git push origin main",
        ] {
            assert!(pre(line).is_none(), "{line}");
        }
        assert_eq!(
            pre("gh pr create --fill"),
            Some(Trigger::PullRequest {
                base: None,
                head: None
            })
        );
        assert!(pre("gh pr view 12").is_none());
        assert!(pre("git status && git log").is_none());
        // A line that stages before it commits is not a plain commit.
        for line in [
            "git add -A && git commit -m x && git push",
            "git add a.py && git rm -r old && git commit -m x",
            "git mv a b && git commit -m x",
            "git add -p && git commit -m x",
        ] {
            assert_eq!(pre(line), Some(commit(false, false)), "{line}");
        }
        assert_eq!(pre("git commit -qam x"), Some(commit(false, false)));
        assert_eq!(pre("git commit -S -m x"), Some(commit(true, false)));
        assert_eq!(pre("git commit -m x -i a.py"), Some(commit(false, false)));
        assert_eq!(pre("git commit -m a -m b"), Some(commit(true, false)));
        assert_eq!(pre("git commit -F msg.txt"), Some(commit(true, false)));
        // An escaped quote inside the message is the message (#436): the
        // gate reviews the index, never lets it through unread.
        assert_eq!(
            pre(r#"git commit -m "vat \"incl\" tax""#),
            Some(commit(true, false))
        );
        assert_eq!(
            detect(Event::PreToolUse, "cd sub/app && git commit -m x").unwrap(),
            Detected {
                trigger: commit(true, false),
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
        // A variable the line sets to a literal directory is that directory
        // (#422); one set from the shell's output is not guessed at.
        for line in [
            "L=/abs/wt; cd $L && git commit -m x",
            "export L=/abs/wt && cd \"${L}\" && git commit -m x",
            "L=/abs; git -C $L/wt commit -m x",
        ] {
            assert_eq!(
                detect(Event::PreToolUse, line).unwrap().dir,
                Some("/abs/wt".to_string()),
                "{line}"
            );
        }
        assert!(pre("L=$(pwd); cd $L && git commit -m x").is_none());
        assert!(pre("L=/a; cd $LX && git commit -m x").is_none());
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
        assert_eq!(post("git diff"), Some(inspection(false)));
        assert_eq!(post("git diff --cached"), Some(inspection(true)));
        assert_eq!(post("git status --short"), Some(inspection(false)));
        assert!(post("git diff HEAD~3..HEAD").is_none());
        assert!(post("git diff HEAD~1 HEAD").is_none());
        assert!(post("git diff main").is_none());
        assert_eq!(post("git diff HEAD"), Some(inspection(false)));
        assert_eq!(post("git diff -- src/"), Some(inspection(false)));
        assert_eq!(post("git diff src/a.py"), Some(inspection(false)));
        // What a commit recorded is read off the reflog, not detected.
        assert!(post("git commit -m x").is_none());
        assert!(pre("git diff").is_none());
        // A status beside a diff only summarises it: the diff decides.
        for line in [
            "git status && git diff --cached",
            "git status --short; git diff --staged",
        ] {
            assert_eq!(post(line), Some(inspection(true)), "{line}");
        }
        assert_eq!(
            post("git status && git commit -m x"),
            Some(inspection(false))
        );
    }

    #[test]
    fn the_first_statement_wins_and_operators_end_it() {
        assert_eq!(
            pre("git add -A; git commit -m 'a; b' | cat"),
            Some(commit(false, false))
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
            Some(commit(true, false))
        );
        assert_eq!(
            pre("git commit -F- <<-\"EOF\" 2>&1 | tail -1\n\tmsg a.py\n\tEOF\ngit push"),
            Some(commit(true, false))
        );
        assert_eq!(
            pre("git add a.py\ngit commit -m x"),
            Some(commit(false, false))
        );
        assert_eq!(pre("git commit \\\n  -m x"), Some(commit(true, false)));
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
            decide(r#"{"tool_input":{"command":"ls"}}"#),
            Err("no-trigger")
        );
    }
}
