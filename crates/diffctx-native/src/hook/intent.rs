//! What a command line says it is about to do, read before it runs and for
//! nothing else: the strict gate's commit, `gh pr create`, the `git diff` or
//! `git status` the agent ran for itself, and the pattern of a text search.
//! It decides nothing about what a line did — that is git's record, read off
//! the reflog once the line ran (#508) — and a directory the line changes
//! into is not followed: an intent is recognised only in the directory the
//! agent started the call in.

use std::path::Path;

use rustc_hash::FxHashMap;

/// What the line is about to do, in the call's own directory.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Intent {
    /// `git commit`: `plain` is a line that stages nothing itself and names
    /// no paths and no `-a`; anything else is asked to stage first, so the
    /// gate never predicts what a command will stage.
    Commit { amend: bool, plain: bool },
    /// `gh pr create`.
    PullRequest {
        base: Option<String>,
        head: Option<String>,
    },
    /// `git diff` / `git status` whose output the agent reads.
    Inspect { staged: bool },
}

/// A text search a line runs.
pub(super) struct Search {
    /// The directory the line changed into before the search, when it did:
    /// the paths a search prints are relative to it.
    pub ran_in: Option<String>,
    /// Where the search looks: its first path, else `ran_in`.
    pub dir: Option<String>,
    pub paths: Vec<String>,
    pub pattern: String,
    pub inverted: bool,
    pub revisions: Vec<String>,
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
                        heredoc_body(&mut chars, &delimiter, strip_tabs, |_, _| {});
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

type Chars<'a> = std::iter::Peekable<std::str::Chars<'a>>;

/// The rest of a `$(…)` after its `(`, appended to `word` up to and including
/// the `)` that closes it: quoted strings and here-document bodies are read
/// as text, so a `)` or `"` inside them closes nothing.
fn skip_substitution(chars: &mut Chars, word: &mut String) {
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
                    heredoc_body(chars, &delimiter, strip_tabs, |line, _| {
                        word.push_str(line);
                        word.push('\n');
                    });
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
fn shell_word(chars: &mut Chars) -> String {
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

/// The rest of a redirection after its first `>`/`<`: more `>`/`&`, then
/// the target word. `true` when it duplicates another descriptor (`>&2`),
/// which the tool still shows.
fn skip_redirection(chars: &mut Chars) -> bool {
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

const SEARCH_TOOLS: &[&str] = &["grep", "egrep", "fgrep", "rg", "ag", "ack"];

/// Search flags whose next word is a value, not the pattern or a path.
const SEARCH_VALUE_FLAGS: &str = "-f --file -A -B -C --context --after-context --before-context -m --max-count -g --glob --iglob -t --type -T --type-not -j --threads -M --max-columns -d --max-depth";

/// What a grep/rg/ag/`git grep` argument list searches for, and where.
struct SearchArgs {
    /// Every `-e` joined as alternation.
    pattern: String,
    paths: Vec<String>,
    /// `-v`, `-L`: it seeks what does not spell the pattern.
    inverted: bool,
    /// `git grep PATTERN REV…`: the trees it searches instead of the work tree.
    revisions: Vec<String>,
}

/// `git grep PATTERN [REV…] [-- PATH…]` names its paths only after `--`;
/// a word between the pattern and `--` is a revision (`origin/main`).
fn search_args(args: &[String], git: bool) -> Option<SearchArgs> {
    let mut patterns: Vec<String> = Vec::new();
    let mut words: Vec<&String> = Vec::new();
    let mut pathspec: Vec<&String> = Vec::new();
    let mut options_end = false;
    let mut inverted = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if options_end {
            if git {
                pathspec.push(a);
            } else {
                words.push(a);
            }
        } else if a == "--" {
            options_end = true;
        } else if a == "-e" || a == "--regexp" {
            patterns.extend(it.next().cloned());
        } else if let Some(p) = a.strip_prefix("--regexp=") {
            patterns.push(p.to_string());
        } else if SEARCH_VALUE_FLAGS.split(' ').any(|f| f == a) {
            it.next();
        } else if a == "--invert-match" || a == "--files-without-match" {
            inverted = true;
        } else if let Some(flags) = a.strip_prefix('-') {
            inverted |= !flags.starts_with('-') && flags.contains(['v', 'L']);
        } else {
            words.push(a);
        }
    }
    let mut words = words.into_iter();
    if patterns.is_empty() {
        patterns.push(words.next()?.clone());
    }
    let (paths, revisions) = if git {
        (pathspec, words.cloned().collect())
    } else {
        (words.collect(), Vec::new())
    };
    Some(SearchArgs {
        pattern: patterns.join("|"),
        paths: paths.into_iter().map(|p| expand_home(p)).collect(),
        inverted,
        revisions,
    })
}

/// Reads a here-document body up to and including its delimiter line,
/// handing `each` every line and whether it is the delimiter's.
fn heredoc_body(
    chars: &mut Chars,
    delimiter: &str,
    strip_tabs: bool,
    mut each: impl FnMut(&str, bool),
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
        let last = if strip_tabs {
            line.trim_start_matches('\t')
        } else {
            &line
        } == delimiter;
        each(&line, last);
        if last || ended {
            return;
        }
    }
}

/// One git, gh or search statement of a command line.
struct Statement {
    verb: String,
    /// The directory a `cd` or `-C` earlier on the line put it in.
    dir: Option<String>,
    args: Vec<String>,
    /// Its output feeds the next command of a pipeline, or goes to a file:
    /// what the agent reads is not this statement's.
    piped_out: bool,
    /// A git (not gh, not a search tool) statement.
    git: bool,
    /// It runs under a directory only the shell could name.
    lost: bool,
    /// Every place the line points the statement at besides the directory
    /// the call started in: a `cd`, `-C`, `--git-dir`, `--work-tree`,
    /// `GIT_DIR` or `GIT_WORK_TREE`.
    named: Vec<Place>,
}

/// Where a line points a git statement.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Place {
    /// A directory, as the line spells it: relative to the call's cwd unless
    /// absolute.
    Dir(String),
    /// A place only the shell could name (`cd "$d"`).
    Unknown,
}

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

fn is_assignment_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The value `name=…` words give the statement at `at`, in the run of
/// assignments just before it (`GIT_DIR=x GIT_WORK_TREE=y git commit`), else
/// a variable the line set earlier.
fn env_of(
    name: &str,
    at: usize,
    ws: &[String],
    vars: &FxHashMap<String, String>,
) -> Option<String> {
    let mut k = at;
    while k > 0 {
        let Some((var, value)) = ws[k - 1].split_once('=') else {
            break;
        };
        if !is_assignment_name(var) {
            break;
        }
        if var == name {
            return Some(substitute(value, vars));
        }
        k -= 1;
    }
    vars.get(name).cloned()
}

/// A repository a statement names by flag or environment, as a place.
fn place_of(value: Option<String>, git_dir: bool, base: Option<&str>) -> Place {
    let Some(value) = value.map(|v| expand_home(&v)) else {
        return Place::Unknown;
    };
    if is_unresolvable(&value) {
        return Place::Unknown;
    }
    // A git directory stands for the work tree beside it.
    let value = match value.strip_suffix("/.git") {
        Some(parent) if git_dir => parent.to_string(),
        _ if git_dir && value == ".git" => ".".to_string(),
        _ => value,
    };
    Place::Dir(join_dir(base, &value))
}

/// Every git, gh or search statement of the command line with the directory
/// it runs in, up to the next shell operator. A statement under a directory
/// only the shell could name (`cd $R`, `cd -`) is marked lost.
fn statements(command: &str) -> Vec<Statement> {
    let ws = words(command);
    let mut found = Vec::new();
    let mut cwd: Option<String> = None;
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
            if is_assignment_name(name) {
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
        // A word with no file name (`/`, `..`) names no program.
        let prog = Path::new(&ws[i])
            .file_name()
            .map_or_else(String::new, |f| f.to_string_lossy().to_string());
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
            if ws[i] == "popd" || target.as_ref().is_none_or(|w| w.starts_with('-')) {
                lost = true;
                i = k;
                continue;
            }
            if let Some(target) = target.map(|w| expand_home(&substitute(&w, &vars))) {
                if is_unresolvable(&target) {
                    lost = true;
                } else if is_absolute_dir(&target) || !lost {
                    cwd = Some(join_dir(cwd.as_deref(), &target));
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
            if !piped {
                found.push(Statement {
                    verb: "grep".to_string(),
                    dir: cwd.clone(),
                    args: ws[i + 1..j]
                        .iter()
                        .filter(|w| *w != STDOUT_TO_FILE)
                        .cloned()
                        .collect(),
                    piped_out: ws.get(j).is_some_and(|w| is_pipe(w)),
                    git: false,
                    lost,
                    named: Vec::new(),
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
            let mut named: Vec<Place> = Vec::new();
            if prog == "git" {
                for name in ["GIT_DIR", "GIT_WORK_TREE"] {
                    if let Some(value) = env_of(name, i, &ws, &vars) {
                        named.push(place_of(Some(value), name == "GIT_DIR", cwd.as_deref()));
                    }
                }
            }
            while j < ws.len() && !is_operator(&ws[j]) {
                let w = &ws[j];
                if prog == "git" {
                    if w == "-C" {
                        if let Some(d) = ws.get(j + 1).map(|d| expand_home(&substitute(d, &vars))) {
                            if is_unresolvable(&d) {
                                dir_lost = true;
                            } else if is_absolute_dir(&d) {
                                dir = Some(d);
                                dir_lost = false;
                            } else {
                                dir = Some(join_dir(dir.as_deref(), &d));
                            }
                        }
                        j += 2;
                        continue;
                    }
                    let (flag, inline) = match w.split_once('=') {
                        Some((f, v)) if matches!(f, "--git-dir" | "--work-tree") => {
                            (f, Some(v.to_string()))
                        }
                        _ => (w.as_str(), None),
                    };
                    if matches!(flag, "--git-dir" | "--work-tree") {
                        let (value, used) = match inline {
                            Some(v) => (Some(v), 1),
                            None => (ws.get(j + 1).cloned(), 2),
                        };
                        let value = value.map(|v| substitute(&v, &vars));
                        named.push(place_of(value, flag == "--git-dir", dir.as_deref()));
                        j += used;
                        continue;
                    }
                    if matches!(w.as_str(), "-c" | "--namespace" | "--config-env") {
                        j += 2;
                        continue;
                    }
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
                if dir_lost {
                    named.push(Place::Unknown);
                } else if let Some(d) = &dir {
                    named.push(Place::Dir(d.clone()));
                }
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
                    git: prog == "git",
                    lost: dir_lost,
                    named,
                });
            }
            i = j;
            continue;
        }
        i += 1;
    }
    found
}

/// Every place the line points a git statement at besides the directory the
/// call started in. It says where the line looks, never what it did.
pub(super) fn places(command: &str) -> Vec<Place> {
    statements(command)
        .into_iter()
        .filter(|st| st.git)
        .flat_map(|st| st.named)
        .collect()
}

/// `~` and `~/…` as the shell expands them, with this process's `HOME`:
/// `cd ~/repo && grep x .` is the most common way an agent names a
/// directory (#377).
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

/// A directory only the shell could expand: a variable, a command
/// substitution, another user's home (`~user`).
fn is_unresolvable(dir: &str) -> bool {
    dir.contains('$') || dir.contains('`') || dir.starts_with('~')
}

/// A directory as the shell line spelled it: `/r` is absolute on every
/// platform here, and the join keeps the line's own separator.
pub(super) fn is_absolute_dir(dir: &str) -> bool {
    dir.starts_with('/') || Path::new(dir).is_absolute()
}

fn join_dir(base: Option<&str>, dir: &str) -> String {
    match base {
        Some(b) if !is_absolute_dir(dir) => format!("{}/{dir}", b.trim_end_matches('/')),
        _ => dir.to_string(),
    }
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

/// What the line is about to do, `after` the tool ran or before it. The
/// first statement that has an intent wins; a `git status` only summarises
/// a diff beside it. A line that stages before it commits is not a plain
/// commit: the gate asks for the staging to run first instead of predicting
/// it.
pub(super) fn of(after: bool, command: &str) -> Option<Intent> {
    let mut stages = false;
    let mut summary = None;
    for st in statements(command) {
        if STAGING_VERBS.contains(&st.verb.as_str()) {
            stages = true;
        }
        let Some(mut intent) = of_statement(after, &st) else {
            continue;
        };
        if st.verb == "status" {
            summary.get_or_insert(intent);
            continue;
        }
        if let Intent::Commit { plain, .. } = &mut intent {
            *plain &= !stages;
        }
        return Some(intent);
    }
    summary
}

fn of_statement(after: bool, st: &Statement) -> Option<Intent> {
    // A statement pointed at another place is not the call's own repository.
    if !st.named.is_empty() {
        return None;
    }
    let (verb, args) = (st.verb.as_str(), &st.args);
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let short_flag = |c: char| short_flags(args).contains(c);
    Some(match (after, verb) {
        (false, "commit") => {
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
            Intent::Commit {
                amend: has("--amend"),
                plain,
            }
        }
        (false, "gh pr") => {
            if args.first().map(String::as_str) != Some("create") {
                return None;
            }
            Intent::PullRequest {
                base: flag_value(args, "--base", "-B"),
                // `owner:branch` is a fork's branch.
                head: flag_value(args, "--head", "-H").map(|h| match h.split_once(':') {
                    Some((_, branch)) => branch.to_string(),
                    None => h,
                }),
            }
        }
        (true, "diff") => {
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
            Intent::Inspect {
                staged: has("--cached") || has("--staged"),
            }
        }
        (true, "status") if !st.piped_out => Intent::Inspect { staged: false },
        _ => return None,
    })
}

/// The first text search of the line: `grep`, `rg`, `ag`, `ack` or
/// `git grep`, the paths it names resolved against the directory the line
/// changed into.
pub(super) fn search(command: &str) -> Option<Search> {
    let Statement {
        dir, args, verb, ..
    } = statements(command)
        .into_iter()
        .find(|st| (st.verb == "grep" || st.verb == "git grep") && !st.lost)?;
    let args = search_args(&args, verb == "git grep")?;
    // A path only the shell can expand (`$OUT/notes.tsv`) is searched
    // nowhere the hook can name: joined to the cwd it read as the cwd's
    // repository, and a grep of a scratch file got that repository's answer.
    if args.paths.iter().any(|p| is_unresolvable(p)) {
        return None;
    }
    let paths: Vec<String> = args
        .paths
        .iter()
        .map(|p| join_dir(dir.as_deref(), p))
        .collect();
    Some(Search {
        ran_in: dir.clone(),
        dir: paths
            .first()
            .map(|p| match p.contains(['*', '?', '[']) {
                true => before_glob(Path::new(p)).to_string_lossy().into_owned(),
                false => p.clone(),
            })
            .or(dir),
        paths,
        pattern: args.pattern,
        inverted: args.inverted,
        revisions: args.revisions,
    })
}

/// A glob searches the directory it starts in (`src/*.rs`, `-- '*.py'`):
/// read as a directory, it named no repository and the search went unanswered.
pub(super) fn before_glob(path: &Path) -> std::path::PathBuf {
    path.components()
        .take_while(|c| !c.as_os_str().to_string_lossy().contains(['*', '?', '[']))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pre(command: &str) -> Option<Intent> {
        of(false, command)
    }

    fn post(command: &str) -> Option<Intent> {
        of(true, command)
    }

    fn inspection(staged: bool) -> Option<Intent> {
        Some(Intent::Inspect { staged })
    }

    /// A commit the gate can review from the index (`plain`), or one that
    /// stages on its own line, names paths or takes `-a`.
    fn commit(plain: bool, amend: bool) -> Option<Intent> {
        Some(Intent::Commit { amend, plain })
    }

    #[test]
    fn a_redirection_is_neither_a_pathspec_nor_an_operator() {
        for command in [
            "git commit -q -m x 2>&1 | tail -3",
            "git commit -m x >/dev/null 2>&1",
            "git commit -m x &> log.txt",
            "git commit -m x 2> err.txt; git log -1",
        ] {
            assert_eq!(pre(command), commit(true, false), "{command}");
        }
        assert_eq!(pre("git commit -m x src/a.py 2>&1"), commit(false, false));
    }

    #[test]
    fn a_commit_is_plain_unless_it_stages_or_names_paths() {
        assert_eq!(pre("git commit -m 'x'"), commit(true, false));
        assert_eq!(pre("git commit -am x"), commit(false, false));
        assert_eq!(pre("git commit --all -m x"), commit(false, false));
        assert_eq!(pre("git commit -m x -- src/a.py"), commit(false, false));
        assert_eq!(
            pre("git commit --pathspec-from-file=list.txt -m x"),
            commit(false, false)
        );
        assert_eq!(pre("git commit -q --amend --no-edit"), commit(true, true));
        assert!(pre("git commit --dry-run").is_none());
        for line in [
            "git add -A && git commit -m x && git push",
            "git add a.py && git rm -r old && git commit -m x",
            "git mv a b && git commit -m x",
            "git add -p && git commit -m x",
        ] {
            assert_eq!(pre(line), commit(false, false), "{line}");
        }
        assert_eq!(pre("git commit -qam x"), commit(false, false));
        assert_eq!(pre("git commit -S -m x"), commit(true, false));
        assert_eq!(pre("git commit -m x -i a.py"), commit(false, false));
        assert_eq!(pre("git commit -m a -m b"), commit(true, false));
        assert_eq!(pre("git commit -F msg.txt"), commit(true, false));
        // An escaped quote inside the message is the message (#436): the
        // gate reviews the index, never lets it through unread.
        assert_eq!(
            pre(r#"git commit -m "vat \"incl\" tax""#),
            commit(true, false)
        );
    }

    #[test]
    fn only_a_pull_request_the_agent_opens_is_an_intent() {
        assert_eq!(
            pre("gh pr create --fill"),
            Some(Intent::PullRequest {
                base: None,
                head: None
            })
        );
        assert_eq!(
            pre("gh pr create --base main -H me:topic"),
            Some(Intent::PullRequest {
                base: Some("main".to_string()),
                head: Some("topic".to_string())
            })
        );
        assert!(pre("gh pr view 12").is_none());
        assert!(pre("git status && git log").is_none());
        // Merges, picks and pushes are read off git's record once they ran,
        // never predicted from the line.
        for line in [
            "git merge feature/x",
            "git cherry-pick abc123",
            "git push origin main",
        ] {
            assert!(pre(line).is_none(), "{line}");
        }
    }

    #[test]
    fn a_directory_the_line_changes_into_is_no_intent_of_the_call() {
        for line in [
            "cd repo && git -C /tmp/r commit -am x",
            "cd sub/app && git commit -m x",
            "cd /r && git -C sub commit -m x",
            "cd ~/x && git commit -m x",
            "git -C \"$REPO\" commit -m x",
            "cd $R && git commit -qm init",
            "cd ~other/x && git commit -m x",
            "git --git-dir=/r/.git --work-tree=/r commit -m x",
            "git --git-dir /r/.git commit -m x",
            "(cd sub && git commit -m x)",
        ] {
            assert!(pre(line).is_none(), "{line}");
        }
        // The subshell's directory ends with it.
        assert_eq!(
            pre("(cd sub && git add a.py); git commit -m x"),
            commit(false, false)
        );
        assert_eq!(
            post("(cd sub && ls); git diff"),
            inspection(false),
            "a directory changed in a subshell ends with it"
        );
    }

    #[test]
    fn the_inspection_verbs_fire_only_after_the_tool_ran() {
        assert_eq!(post("git diff"), inspection(false));
        assert_eq!(post("git diff --cached"), inspection(true));
        assert_eq!(post("git status --short"), inspection(false));
        assert!(post("git diff HEAD~3..HEAD").is_none());
        assert!(post("git diff HEAD~1 HEAD").is_none());
        assert!(post("git diff main").is_none());
        assert_eq!(post("git diff HEAD"), inspection(false));
        assert_eq!(post("git diff -- src/"), inspection(false));
        assert_eq!(post("git diff src/a.py"), inspection(false));
        // What a commit recorded is read off the reflog, not detected.
        assert!(post("git commit -m x").is_none());
        assert!(pre("git diff").is_none());
        // A status beside a diff only summarises it: the diff decides.
        for line in [
            "git status && git diff --cached",
            "git status --short; git diff --staged",
        ] {
            assert_eq!(post(line), inspection(true), "{line}");
        }
        assert_eq!(post("git status && git commit -m x"), inspection(false));
    }

    #[test]
    fn the_first_statement_wins_and_operators_end_it() {
        assert_eq!(
            pre("git add -A; git commit -m 'a; b' | cat"),
            commit(false, false)
        );
        assert!(pre("git log | grep commit").is_none());
    }

    #[test]
    fn a_heredoc_body_is_text_and_a_newline_ends_the_statement() {
        assert_eq!(
            pre("git commit -q -F - <<'EOF'\nfix: don't x; y | z\n\nbody `a` & b\nEOF"),
            commit(true, false)
        );
        assert_eq!(
            pre("git commit -F- <<-\"EOF\" 2>&1 | tail -1\n\tmsg a.py\n\tEOF\ngit push"),
            commit(true, false)
        );
        assert_eq!(pre("git add a.py\ngit commit -m x"), commit(false, false));
        assert_eq!(pre("git commit \\\n  -m x"), commit(true, false));
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
    fn a_line_names_the_places_it_points_git_at() {
        let dir = |d: &str| Place::Dir(d.to_string());
        assert!(places("git commit -m x").is_empty());
        assert!(places("cd /r && ls").is_empty());
        assert!(places("gh pr create --fill").is_empty());
        assert_eq!(places("cd /r && git commit -m x"), [dir("/r")]);
        assert_eq!(places("git -C /r commit -m x"), [dir("/r")]);
        assert_eq!(
            places("(cd sub && git add a.py); git commit -m x"),
            [dir("sub")]
        );
        assert_eq!(places("R=/r && cd \"$R\" && git commit -m x"), [dir("/r")]);
        assert_eq!(
            places("git --git-dir=/r/.git --work-tree=/r commit -m x"),
            [dir("/r"), dir("/r")]
        );
        assert_eq!(
            places("GIT_DIR=/r/.git GIT_WORK_TREE=/r git commit -m x"),
            [dir("/r"), dir("/r")]
        );
        assert_eq!(
            places("git --git-dir .git --work-tree . add a"),
            [dir("."), dir(".")]
        );
        assert_eq!(
            places("for d in a b; do ( cd \"$d\" && git commit -m x ); done"),
            [Place::Unknown]
        );
        assert_eq!(places("git -C \"$REPO\" commit -m x"), [Place::Unknown]);
        assert_eq!(
            places("L=$(pwd); cd $L && git commit -m x"),
            [Place::Unknown]
        );
    }

    #[test]
    fn a_search_names_where_it_looks_and_what_it_skips() {
        let found = |c: &str| search(c).map(|s| (s.dir, s.paths, s.inverted, s.revisions));
        assert_eq!(
            search("cd /r && rg -n total shop/").unwrap().dir,
            Some("/r/shop/".to_string())
        );
        assert_eq!(
            search("cd /r && grep -rn total shop tests").unwrap().paths,
            ["/r/shop", "/r/tests"]
        );
        assert_eq!(
            found("grep -rvn total ."),
            Some((Some(".".to_string()), vec![".".to_string()], true, vec![]))
        );
        assert!(search("git grep -L total").unwrap().inverted);
        assert_eq!(
            search("git grep -n total origin/main -- src")
                .unwrap()
                .revisions,
            ["origin/main"]
        );
        assert!(!search("rg -n -C 3 total src/").unwrap().inverted);
        assert_eq!(
            search(r"git grep -n -e '\btotal\b' -- '*.py'")
                .unwrap()
                .pattern,
            r"\btotal\b"
        );
        assert!(search("git log | grep total").is_none());
        assert!(search("cd $R && grep -rn total .").is_none());
        assert!(search("grep -rn total $OUT/notes.tsv").is_none());
        assert!(search("cargo test").is_none());
    }
}
