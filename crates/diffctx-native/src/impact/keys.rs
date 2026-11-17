//! What a config line spells that a reader of the config may spell back, as text:
//! the words and the values a diff of a config touched, found the same way on the
//! changed side and on the reader's. The reader side is the token index's
//! (`Cells::spelled_lines`); this is the vocabulary both sides agree on.

use std::path::Path;

use super::ScoredState;
use crate::decide::Cells;

/// The terms of one line of a config: its key-shaped words and its value phrases.
pub(super) fn terms(line: &str) -> Vec<String> {
    changed_words(line).chain(value_phrases(line)).collect()
}

/// A value that spells more than a word: a selector, a path, an address, an
/// image. It is searched whole, so it has to be long enough, carry letters
/// and a structure of its own, and start as a value does — `{"retries": 5}`
/// is the file's own syntax, `Hello world` is prose.
pub(super) fn phrase_like(token: &str) -> bool {
    token.chars().count() >= 8
        && token
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || matches!(c, '/' | '.' | '_'))
        && token.chars().filter(|c| c.is_alphabetic()).count() >= 4
        && token.chars().any(|c| {
            matches!(
                c,
                '{' | '}' | '[' | ']' | '(' | ')' | '=' | '/' | ':' | '@' | '"' | '\''
            )
        })
}

/// Whether a search term is a value phrase and not a key word.
pub(super) fn is_phrase(term: &str) -> bool {
    term.chars()
        .any(|c| !(c.is_alphanumeric() || c == '_' || c == '-'))
}

/// The whitespace-separated pieces of a line, kept whole where whitespace
/// belongs to the piece: inside a quoted string, and inside the brackets of
/// a selector or a call (`up{job="a", pod="b"}`) — a bracket after a word
/// character. A bracket after whitespace or another bracket is the file's own
/// flow syntax and splits like any other text.
fn split_tokens(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start: Option<usize> = None;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut brackets: Vec<bool> = Vec::new();
    let mut attached = 0usize;
    let mut prev: Option<char> = None;
    for (i, c) in text.char_indices() {
        if quote.is_none() && c.is_whitespace() && attached == 0 {
            if let Some(s) = start.take() {
                tokens.push(&text[s..i]);
            }
            prev = None;
            continue;
        }
        if start.is_none() {
            start = Some(i);
            if c == '\'' || c == '"' {
                quote = Some(c);
                prev = Some(c);
                continue;
            }
        }
        match quote {
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '{' | '[' | '(' => {
                    let on_word = prev.is_some_and(|p| p.is_alphanumeric() || p == '_');
                    brackets.push(on_word);
                    if on_word {
                        attached += 1;
                    }
                }
                '}' | ']' | ')' => {
                    if brackets.pop() == Some(true) {
                        attached = attached.saturating_sub(1);
                    }
                }
                _ => {}
            },
        }
        prev = Some(c);
    }
    if let Some(s) = start {
        tokens.push(&text[s..]);
    }
    tokens
}

/// `token` without the flow syntax around it: the brackets of an enclosing
/// list or mapping, the comma after it, the colon of a key.
fn trim_flow(token: &str) -> &str {
    let mut t = token.trim_start_matches(['[', '{']);
    loop {
        let cut = t.trim_end_matches([',', ';']);
        let opens = cut.chars().filter(|c| matches!(c, '{' | '[' | '(')).count();
        let closes = cut.chars().filter(|c| matches!(c, '}' | ']' | ')')).count();
        if closes > opens && cut.ends_with(['}', ']', ')']) {
            t = &cut[..cut.len() - 1];
        } else {
            return cut.strip_suffix(':').unwrap_or(cut);
        }
    }
}

/// `key=value` with a key-shaped head.
fn assignment(token: &str) -> Option<(&str, &str)> {
    let (head, value) = token.split_once('=')?;
    let key = !head.is_empty()
        && head
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-'));
    key.then_some((head, value))
}

/// The values a config line spells that are worth searching whole, found the
/// same way on the changed side and on the reader's: `expr: 'up{job="a"} == 0'`
/// gives `up{job="a"}`, `url=http://db:5432/app` gives `http://db:5432/app`.
pub(super) fn value_phrases(line: &str) -> Vec<String> {
    fn collect(text: &str, depth: usize, out: &mut Vec<String>) {
        for raw in split_tokens(text) {
            let token = trim_flow(raw);
            let inner = match token.chars().next() {
                Some(q @ ('\'' | '"')) if token.len() >= 2 && token.ends_with(q) => {
                    Some(&token[1..token.len() - 1])
                }
                _ => assignment(token).map(|(_, value)| value),
            };
            match inner {
                Some(inner) if depth < 2 => collect(inner, depth + 1, out),
                Some(_) => {}
                None if phrase_like(token) => out.push(token.to_string()),
                None => {}
            }
        }
    }
    let mut out = Vec::new();
    collect(line.trim().trim_start_matches("- "), 0, &mut out);
    out
}

/// Every word of a line a reader spells: the needle is what is narrow.
pub(super) fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .map(|w| w.trim_matches('-'))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
}

pub(super) fn key_words(text: &str) -> impl Iterator<Item = String> + '_ {
    words(text).filter(|w| key_like(w))
}

/// The key-shaped words a changed line offers to search. A value that is
/// spelled whole (an image, a path, an address: `phrase_like`) is searched whole
/// and gives no words of its own, as `1-debian13` of `app:2.14.1-debian13` is no
/// key (#707); a quoted sentence is prose and gives none either (#700). A reader's
/// line is read for every word (`words`): it is the needle that is narrow.
fn changed_words(text: &str) -> std::vec::IntoIter<String> {
    spelled_words(text, |word| key_words(word).collect())
}

/// The words a line took out of a config that no line kept (`removed` the lines
/// it lost, `added` the lines the file now has): a renamed key or value of any length,
/// `pgdb`, `web`, `pgdb5`. The change dropped the name, so what still spells it
/// is stale; a name short or plain enough to be everyone's is searched all the
/// same, and the search gives it up when too many files spell it.
pub(super) fn dropped_words<'a>(
    removed: impl Iterator<Item = &'a str>,
    added: impl Iterator<Item = &'a str>,
) -> Vec<String> {
    let kept: rustc_hash::FxHashSet<String> = added
        .flat_map(|line| spelled_words(&without_sentences(line), plain_words))
        .collect();
    let mut dropped: Vec<String> = removed
        .flat_map(|line| spelled_words(&without_sentences(line), plain_words))
        .filter(|w| !kept.contains(w))
        .collect();
    dropped.sort();
    dropped.dedup();
    dropped
}

/// Every word of `text` an identifier-shaped reader may spell back: three
/// characters or more, a letter among them, no stopword.
fn plain_words(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .map(|w| w.trim_matches('-'))
        .filter(|w| {
            w.chars().count() >= 3
                && w.chars().any(|c| c.is_ascii_alphabetic())
                && !crate::stopwords::CODE_STOPWORDS.contains(&w.to_lowercase())
        })
        .map(str::to_string)
        .collect()
}

/// The words of a config line outside its sentences and its whole-value phrases,
/// `words` choosing which of a token's words count.
fn spelled_words(text: &str, words: fn(&str) -> Vec<String>) -> std::vec::IntoIter<String> {
    fn collect(text: &str, depth: usize, out: &mut Vec<String>, words: fn(&str) -> Vec<String>) {
        for raw in split_tokens(text) {
            let token = trim_flow(raw);
            match token.chars().next() {
                Some(q @ ('\'' | '"')) if token.len() >= 2 && token.ends_with(q) => {
                    let inner = &token[1..token.len() - 1];
                    if depth < 2 {
                        collect(inner, depth + 1, out, words);
                    }
                }
                _ => match assignment(token) {
                    Some((head, value)) if depth < 2 => {
                        out.extend(words(head));
                        collect(value, depth + 1, out, words);
                    }
                    _ if phrase_like(token) => {}
                    _ => out.extend(words(token)),
                },
            }
        }
    }
    let mut out = Vec::new();
    collect(&without_sentences(text), 0, &mut out, words);
    out.into_iter()
}

/// `line` with each quoted string that is a sentence blanked out: a quote opens
/// where a value starts, never inside a word (`don't`).
fn without_sentences(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.char_indices().peekable();
    let mut prev = ' ';
    while let Some((at, c)) = chars.next() {
        let opens = matches!(c, '"' | '\'')
            && (prev.is_whitespace() || matches!(prev, '[' | '(' | ',' | ':' | '='));
        if opens {
            let mut escaped = false;
            let close = chars.clone().find_map(|(i, d)| {
                let end = d == c && !escaped;
                escaped = d == '\\' && !escaped;
                end.then_some(i)
            });
            if let Some(close) = close {
                if super::mentions::is_sentence(&line[at + 1..close]) {
                    while chars.next().is_some_and(|(i, _)| i < close) {}
                    out.push(' ');
                    prev = ' ';
                    continue;
                }
            }
        }
        out.push(c);
        prev = c;
    }
    out
}

/// Shaped like a key a reader spells back: joined words, a constant's
/// capitals or one long word. `Hello` and `retries` are not.
fn key_like(word: &str) -> bool {
    let chars: Vec<char> = word.chars().collect();
    if chars.len() < 4
        || !chars.iter().any(|c| c.is_ascii_alphabetic())
        || crate::stopwords::CODE_STOPWORDS.contains(&word.to_lowercase())
    {
        return false;
    }
    let joined = (word.contains('_') || word.contains('-')) && chars.len() >= 6;
    let camel = chars
        .windows(2)
        .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase());
    let constant = chars
        .iter()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_');
    let long = chars.len() >= 8 && chars.iter().all(char::is_ascii_lowercase);
    joined || camel || constant || long
}

/// Where a changed name is spelled as a literal: a string a program resolves
/// by name (`getattr(m, "compute")`, `"pkg.core:entry"`, `@MethodSource("A#m")`,
/// `init-method="init"`), a value of a config or a flag (`controller: A::show`,
/// `--target stage`, `TF_VAR_name`), a member call of a template, a name inside
/// a query string, or the head of a name a program completes at run time
/// (`"cmd_$1"`). A plain word is no key, and a plain word as a literal is the
/// whole string or the tail of a qualified one, never a word of a sentence.
/// What it finds is a candidate to name, never a binding.
#[derive(Default)]
pub(super) struct LiteralNames {
    rows: rustc_hash::FxHashMap<String, Vec<(String, u32)>>,
}

pub(super) const MIN_LITERAL_NAME: usize = 4;
const MIN_CALL_NAME: usize = 2;
const NAMES_PER_SEARCH: usize = 40;
const SQL_VERBS: &[&str] = &[
    "select", "insert", "update", "delete", "with", "call", "merge",
];
/// The calls and commands that only write their argument out: a name they are
/// handed is printed for a reader, never resolved by the program.
const PRINTERS: &[&str] = &[
    "print", "println", "printf", "fprintf", "puts", "echo", "write", "writeln", "log", "debug",
    "info", "warn", "warning", "error", "fatal", "trace", "eprint", "eprintln", "print_r",
    "var_dump",
];
/// The calls whose string argument titles a test or a suite, or is one piece of a
/// path: a name it spells is what the title is about or a file's name, never
/// something the program resolves.
const TITLES: &[&str] = &[
    "describe", "it", "test", "suite", "context", "specify", "join",
];
/// A fixed prefix an environment variable spells a name with.
const NAME_PREFIXES: &[&str] = &["TF_VAR_"];
const TEMPLATE_GLOBS: &[&str] = &[
    ":(glob,icase)**/*.html",
    ":(glob,icase)**/*.htm",
    ":(glob,icase)**/*.xhtml",
    ":(glob,icase)**/*.jsp",
    ":(glob,icase)**/*.ftl",
    ":(glob,icase)**/*.vm",
    ":(glob,icase)**/*.twig",
    ":(glob,icase)**/*.erb",
    ":(glob,icase)**/*.haml",
    ":(glob,icase)**/*.slim",
    ":(glob,icase)**/*.mustache",
    ":(glob,icase)**/*.hbs",
];

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn qualifier(c: char) -> bool {
    matches!(c, '.' | ':' | '#' | '/' | '\\')
}

fn literal_name(name: &str) -> bool {
    name.chars().count() >= MIN_LITERAL_NAME && name.chars().all(|c| word_char(c) || c == '-')
}

/// What a search reads a name as.
#[derive(Clone, Copy, PartialEq)]
enum Shape {
    /// `name(` in a file no reader parses.
    Call,
    /// A name spelled as a literal in code or config.
    Literal,
    /// A name too short for the literal forms, handed as a string to a
    /// reflective accessor or used as an index key in code.
    Argument,
}

/// The lines that quote one of `names` whole.
fn quoted_pattern(names: &[&str]) -> String {
    let all = names
        .iter()
        .map(|n| regex::escape(n))
        .collect::<Vec<_>>()
        .join("|");
    format!(r#"["'`]({all})["'`]"#)
}

/// A name a call in an unread file can carry: `name(` is selective enough to
/// spare the length floor of the other searches.
fn call_name(name: &str) -> bool {
    name.chars().count() >= MIN_CALL_NAME && name.chars().all(word_char)
}

/// The calls whose string argument names a member, per language: where a name
/// too short to search plain is read as one.
fn accessors(language: &str) -> &'static [&'static str] {
    match language {
        "python" => &[
            "getattr",
            "hasattr",
            "setattr",
            "delattr",
            "attrgetter",
            "methodcaller",
        ],
        "go" => &["MethodByName", "FieldByName"],
        "ruby" => &["send", "public_send", "__send__", "method", "respond_to?"],
        "javascript" | "jsx" | "typescript" | "tsx" => {
            &["Reflect.get", "Reflect.apply", "Reflect.has"]
        }
        "java" | "kotlin" => &[
            "getMethod",
            "getDeclaredMethod",
            "getField",
            "getDeclaredField",
        ],
        "php" => &[
            "call_user_func",
            "call_user_func_array",
            "method_exists",
            "property_exists",
        ],
        _ => &[],
    }
}

/// Whether the quoted string opening at `from` is the key of an indexed call:
/// `obj["name"](`.
fn indexed_call(text: &str, from: usize, to: usize) -> bool {
    text[..from - 1].trim_end().ends_with('[')
        && text[to + 1..]
            .trim_start()
            .strip_prefix(']')
            .is_some_and(|rest| rest.trim_start().starts_with('('))
}

/// Whether `before` ends inside the open parenthesis of a call to `accessor`.
fn inside_call(before: &str, accessor: &str) -> bool {
    before.match_indices(accessor).any(|(at, _)| {
        let rest = before[at + accessor.len()..].trim_start();
        !before[..at].chars().next_back().is_some_and(word_char)
            && rest.strip_prefix('(').is_some_and(|args| {
                let mut depth = 0i32;
                args.chars().all(|c| {
                    depth += i32::from(c == '(') - i32::from(c == ')');
                    depth >= 0
                })
            })
    })
}

/// Whether `text`, a line of code in `language`, hands `name` as a string to a
/// reflective accessor or indexes a call by it.
fn reflects(text: &str, name: &str, language: &str) -> bool {
    quoted_spans(text).into_iter().any(|(from, to)| {
        &text[from..to] == name
            && (indexed_call(text, from, to)
                || accessors(language)
                    .iter()
                    .any(|accessor| inside_call(&text[..from - 1], accessor)))
    })
}

/// The heads of `name` a program can complete: up to each `_`.
fn dynamic_heads(name: &str) -> impl Iterator<Item = &str> {
    name.match_indices('_')
        .map(|(at, _)| &name[..=at])
        .filter(move |head| head.len() >= MIN_LITERAL_NAME && head.len() < name.len())
}

/// `[start, end)` of the quoted strings of a line, quotes excluded.
fn quoted_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut open: Option<(char, usize)> = None;
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        match open {
            Some((q, from)) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    spans.push((from, i));
                    open = None;
                }
            }
            None if matches!(c, '"' | '\'' | '`') => open = Some((c, i + 1)),
            None => {}
        }
    }
    spans
}

/// What a file's strings and values are read as: source code reads a string as
/// a callee only as an argument (`getattr(m, "f")`, `@MethodSource("A#m")`) or
/// as a qualified path in a list.
#[derive(Clone, Copy)]
struct Reading {
    /// A config or a command file: its values and flags name what they run.
    values: bool,
    /// PHP: an array value, a constant or an assignment holds a callable string,
    /// and `Class@method` is a route action.
    php: bool,
    /// Go: a `const`, a `var` or a `:=` holds a callable string, and a lookup's argument is one.
    go: bool,
}

impl Reading {
    fn of(path: &str) -> Option<Self> {
        let code = super::is_code_file(path);
        let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
        let values = matches!(
            super::extension(path).as_str(),
            "yaml"
                | "yml"
                | "toml"
                | "ini"
                | "cfg"
                | "properties"
                | "conf"
                | "env"
                | "xml"
                | "mk"
                | "html"
                | "htm"
                | "svg"
                | "xhtml"
                | "jsp"
                | "ftl"
                | "vm"
                | "twig"
                | "erb"
                | "haml"
                | "slim"
        ) || !name.contains('.')
            || name.starts_with("dockerfile");
        let php = matches!(super::extension(path).as_str(), "php" | "phtml" | "inc");
        let go = super::extension(path) == "go";
        (code || values).then_some(Self { values, php, go })
    }
}

/// Whether the text before a string is `#define NAME`: the string is the macro's value.
fn macro_value(before: &str) -> bool {
    let mut words = before.split_whitespace();
    words.next() == Some("#define") && words.next().is_some() && words.next().is_none()
}

/// Whether the text ahead of a string ends in a call that only prints it, titles a
/// test with it or joins it into a path: `print(`, `console.log(`, `echo -n `,
/// `describe(`, `path.join(dir, `.
fn printed(before: &str) -> bool {
    let inert = |callee: &str| {
        let callee = callee
            .rsplit(['.', ':'])
            .next()
            .unwrap_or(callee)
            .to_lowercase();
        PRINTERS.contains(&callee.as_str()) || TITLES.contains(&callee.as_str())
    };
    let before = before.trim_end();
    let opened = before.strip_suffix('(').unwrap_or(before).trim_end();
    opened
        .split_whitespace()
        .rev()
        .find(|token| !token.starts_with('-'))
        .is_some_and(inert)
        || open_call(before).is_some_and(inert)
}

/// The callee of the call still open at the end of `before`: `path.join` in
/// `path.join(os.tmpdir(), `.
fn open_call(before: &str) -> Option<&str> {
    let mut depth = 0usize;
    for (at, c) in before.char_indices().rev() {
        match c {
            ')' | ']' | '}' => depth += 1,
            '(' | '[' | '{' if depth > 0 => depth -= 1,
            '(' => {
                let head = &before[..at];
                let start = head
                    .char_indices()
                    .rev()
                    .find(|(_, c)| !(word_char(*c) || matches!(c, '.' | ':')))
                    .map_or(0, |(i, c)| i + c.len_utf8());
                return Some(&head[start..]).filter(|callee| !callee.is_empty());
            }
            '[' | '{' => return None,
            _ => {}
        }
    }
    None
}

/// Whether `before`, the text ahead of a string, ends in an argument's label
/// (`(module: `, `, type: `, `(name=`), not an assignment or a mapping key.
fn labeled(before: &str) -> bool {
    let rest = before
        .trim_end()
        .trim_end_matches([':', '='])
        .trim_end()
        .trim_end_matches(word_char)
        .trim_end();
    rest.ends_with(['(', ','])
}

/// Whether `text`, a docblock line, names `name` as a PHPUnit data provider:
/// `@dataProvider provide`, `@dataProvider Calc::cases`.
fn data_provider_named(text: &str, name: &str) -> bool {
    text.split_once("@dataProvider")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .is_some_and(|provider| provider.rsplit("::").next() == Some(name))
}

/// Whether `text` spells `name` as a literal in a file read as `reading`.
fn spells_literally(text: &str, name: &str, reading: Reading, held: &[String]) -> bool {
    let spans = quoted_spans(text);
    let sql = name.chars().any(|c| c == '_' || c.is_ascii_digit());
    text.match_indices(name).any(|(at, _)| {
        let end = at + name.len();
        let before = text[..at].chars().next_back();
        let after = text[end..].chars().next();
        let prefixed = NAME_PREFIXES.iter().any(|p| text[..at].ends_with(p));
        if (before.is_some_and(word_char) && !prefixed) || after.is_some_and(word_char) {
            return false;
        }
        if prefixed {
            return true;
        }
        if let Some(&(from, to)) = spans.iter().find(|(f, t)| *f <= at && end <= *t) {
            let inside = &text[from..to];
            // An entry point is `key = module:target`: the target is what it names.
            let head = text[from..at]
                .split_once('=')
                .filter(|(key, _)| {
                    !key.trim().is_empty() && !key.trim().contains(char::is_whitespace)
                })
                .map_or(&text[from..at], |(_, target)| target.trim_start());
            let tail = &text[end..to];
            let qualified = !head.is_empty();
            let lead = text[..from - 1].trim_end().chars().next_back();
            // A qualified path with nothing after it is a value: a list entry on
            // a line of its own, a mapping's, an assignment's. `"a.f(x)"` is prose.
            let value =
                qualified && tail.is_empty() && matches!(lead, None | Some('{' | ':' | '='));
            let php_placed = held.iter().any(|h| h == inside);
            let indexed = !qualified && indexed_call(text, from, to);
            let placed = (reading.values
                || indexed
                || matches!(lead, Some('(' | ','))
                || (matches!(lead, Some(':' | '=')) && labeled(&text[..from - 1]))
                || (qualified && lead == Some('['))
                || macro_value(&text[..from - 1])
                || value
                || php_placed)
                && !printed(&text[..from - 1]);
            let reference = placed
                && !head.contains(char::is_whitespace)
                && !tail.contains(char::is_whitespace)
                && (!qualified
                    || head.ends_with(qualifier)
                    || (reading.php && head.ends_with('@')))
                && (tail.is_empty() || tail.starts_with(['(', '=', ':', '[']));
            let query = sql
                && inside
                    .split_whitespace()
                    .next()
                    .is_some_and(|w| SQL_VERBS.contains(&w.to_lowercase().as_str()));
            // `"{.name}".format(user)` reads the attribute of its argument.
            let formatted = head.contains('{')
                && head.ends_with('.')
                && tail.starts_with(['}', '!', ':', '.', '['])
                && text[to + 1..].starts_with(".format(");
            return reference || query || formatted;
        }
        if !reading.values {
            return false;
        }
        if before == Some('.') && after == Some('(') {
            return true;
        }
        // A value of a config or the argument of a flag: the tail of the token
        // that follows `key:`, `key=` or `--flag`.
        let token_start = text[..at].rfind(char::is_whitespace).map_or(0, |i| i + 1);
        let token_head = &text[token_start..at];
        let before_token = text[..token_start].trim_end();
        let qualified = token_head.is_empty() || token_head.ends_with(qualifier);
        let value = before_token.ends_with([':', '='])
            || before_token
                .rsplit(char::is_whitespace)
                .next()
                .is_some_and(|w| w.starts_with('-') && w.len() > 1)
            || token_head.contains('=');
        qualified
            && value
            && after.is_none_or(|c| c.is_whitespace() || matches!(c, ',' | ';' | '\'' | '"' | '='))
    }) || dynamic_heads(name).any(|head| {
        text.match_indices(head).any(|(at, _)| {
            let rest = &text[at + head.len()..];
            !text[..at].chars().next_back().is_some_and(word_char)
                && (["$", "%s", "#{", "{"].iter().any(|m| rest.starts_with(m))
                    || rest
                        .strip_prefix(['"', '\'', '`'])
                        .is_some_and(|after| after.trim_start().starts_with('+')))
        })
    })
}

/// The grep selecting the lines that can hold `names` as a literal, over the
/// forms `spells_literally` reads back.
fn literal_pattern(names: &[&str]) -> String {
    let alt = |words: Vec<&str>| {
        words
            .into_iter()
            .map(regex::escape)
            .collect::<Vec<_>>()
            .join("|")
    };
    let all = alt(names.to_vec());
    let query = alt(names
        .iter()
        .copied()
        .filter(|n| n.chars().any(|c| c == '_' || c.is_ascii_digit()))
        .collect());
    let heads = alt(names.iter().flat_map(|n| dynamic_heads(n)).collect());
    let verbs: String = SQL_VERBS
        .iter()
        .map(|v| {
            v.chars()
                .map(|c| format!("[{}{}]", c, c.to_ascii_uppercase()))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("|");
    let mut forms = vec![
        format!(
            r#"["'`]([^"'`[:space:]=]+[[:space:]]*=[[:space:]]*)?([^"'`[:space:]]*[.:#/\\@])?({all})["'`(=:\[}}]"#
        ),
        format!(r#"@dataProvider[[:space:]]+([^[:space:]]*::)?({all})([[:space:]*]|$)"#),
        format!(r#"[:=][[:space:]]*([^[:space:]"']*[.:#/\\])?({all})([[:space:],;'"]|$)"#),
        format!(r#"(^|[[:space:]])--?[[:alnum:]-]+[ =]['"]?({all})(['"=[:space:]]|$)"#),
    ];
    if !query.is_empty() {
        forms.push(format!(
            r#"["'`][[:space:]]*({verbs})[[:space:]]([^"'`]*[^[:alnum:]_])?({query})[^[:alnum:]_]"#
        ));
        for prefix in NAME_PREFIXES {
            forms.push(format!("{prefix}({query})"));
        }
    }
    if !heads.is_empty() {
        forms.push(format!(r#"({heads})(\$|%s|#\{{|\{{|["'`][[:space:]]*\+)"#));
    }
    forms.join("|")
}

/// The lines that call one of `names` — `name(` — where a program of any
/// language would: the search leaves out the extensions a reader parses.
fn call_pattern(names: &[&str]) -> String {
    let all = names
        .iter()
        .map(|n| regex::escape(n))
        .collect::<Vec<_>>()
        .join("|");
    format!(r"(^|[^[:alnum:]_$])({all})[[:space:]]*[(]")
}

/// Whether `text` calls `name`: the name as a whole word, then `(`.
fn called(text: &str, name: &str) -> bool {
    text.match_indices(name).any(|(at, _)| {
        !text[..at]
            .chars()
            .next_back()
            .is_some_and(|c| word_char(c) || c == '$')
            && text[at + name.len()..].trim_start().starts_with('(')
    })
}

/// A file no reader parses: its name names no language, it is no data, and an
/// extensionless one has no shebang. What it spells as a call is a use the
/// engine cannot read.
fn unread_file(state: &ScoredState, path: &str) -> bool {
    crate::languages::get_language_for_file(path).is_none()
        && !super::mentions::has_data_extension(path)
        && (Path::new(path).extension().is_some()
            || state
                .run
                .source()
                .read_to_string(&state.root_dir.join(path))
                .is_none_or(|text| crate::languages::sniff_language(path, &text).is_none()))
}

/// A definition's own lines: (file, literal name, lines). A literal inside its
/// own span is the definition's text, never a use of it.
pub(super) type OwnSpan = (String, String, (u32, u32));

impl LiteralNames {
    /// The lines of the tree at the range's head that spell one of `names` as a
    /// literal, in any file — one the change edits and the definition's own
    /// included — outside the definitions' own spans, and as a call in a file
    /// no reader parses.
    pub(super) fn build(
        state: &ScoredState,
        cells: &Cells,
        diff_range: Option<&str>,
        names: &[&str],
        own: &[OwnSpan],
    ) -> Self {
        let mut wanted: Vec<&str> = names
            .iter()
            .copied()
            .filter(|n| literal_name(n) || call_name(n))
            .collect();
        wanted.sort_unstable();
        wanted.dedup();
        if wanted.is_empty() {
            return Self::default();
        }
        let head = super::head_of(diff_range);
        let source = state.run.source();
        let mapped = crate::languages::EXTENSION_TO_LANGUAGE
            .keys()
            .map(|ext| format!(":(exclude,glob,icase)**/*{ext}"))
            .collect::<Vec<_>>();
        let unmapped = mapped.iter().map(String::as_str).collect::<Vec<_>>();
        let mut rows: rustc_hash::FxHashMap<String, Vec<(String, u32)>> =
            rustc_hash::FxHashMap::default();
        for chunk in wanted.chunks(NAMES_PER_SEARCH) {
            if !state.run.check() {
                break;
            }
            let (long, short): (Vec<&str>, Vec<&str>) =
                chunk.iter().copied().partition(|n| literal_name(n));
            let callable: Vec<&str> = chunk.iter().copied().filter(|n| call_name(n)).collect();
            let mut found = Vec::new();
            if !callable.is_empty() {
                found.push((call_pattern(&callable), unmapped.clone(), Shape::Call));
            }
            if !long.is_empty() {
                let templates = format!(
                    r"[.]({})[(]",
                    long.iter()
                        .map(|n| regex::escape(n))
                        .collect::<Vec<_>>()
                        .join("|")
                );
                found.push((literal_pattern(&long), Vec::new(), Shape::Literal));
                found.push((templates, TEMPLATE_GLOBS.to_vec(), Shape::Literal));
            }
            if !short.is_empty() {
                found.push((quoted_pattern(&short), Vec::new(), Shape::Argument));
            }
            for (pattern, pathspec, shape) in found {
                let grepped = super::read_or_note(
                    state,
                    crate::grep::grep_lines_matching(
                        &state.root_dir,
                        &source,
                        &pattern,
                        head.as_deref(),
                        &pathspec,
                    ),
                );
                state.run.note_withheld(grepped.withheld);
                for (path, line, text) in state.run.bounded(&grepped.rows) {
                    if super::is_document(path)
                        || super::mentions::selects_paths(path)
                        || (super::mentions::commented_in(path, text)
                            && !Reading::of(path)
                                .is_some_and(|r| r.php && text.contains("@dataProvider")))
                        || matches!(
                            super::extension(path).as_str(),
                            "diff" | "patch" | "lock" | "tex" | "bib" | "csv" | "tsv" | "log"
                        )
                    {
                        continue;
                    }
                    let reading = Reading::of(path);
                    let language = crate::languages::get_language_for_file(path);
                    let eligible = match shape {
                        Shape::Call => unread_file(state, path),
                        Shape::Literal => reading.is_some(),
                        Shape::Argument => reading.is_some_and(|r| !r.values) && language.is_some(),
                    };
                    if !eligible {
                        continue;
                    }
                    let inside = |name: &str| {
                        own.iter().any(|(file, own_name, (from, to))| {
                            file == path && own_name == name && (*from..=*to).contains(line)
                        })
                    };
                    let held = match reading {
                        Some(r) if (r.php || r.go) && shape == Shape::Literal => cells
                            .of(path)
                            .map_or_else(Vec::new, |cell| cell.callable_strings_at(path, *line)),
                        _ => Vec::new(),
                    };
                    let hit = |name: &str| match (shape, reading, language) {
                        (Shape::Call, ..) => call_name(name) && called(text, name),
                        (Shape::Literal, Some(reading), _) => {
                            literal_name(name)
                                && (spells_literally(text, name, reading, &held)
                                    || (reading.php && data_provider_named(text, name)))
                        }
                        (Shape::Argument, _, Some(language)) => {
                            !literal_name(name) && reflects(text, name, language)
                        }
                        _ => false,
                    };
                    for name in chunk.iter().filter(|n| !inside(n) && hit(n)) {
                        let named = rows.entry((*name).to_string()).or_default();
                        if !named.iter().any(|(p, _)| p == path) {
                            named.push((path.clone(), *line));
                        }
                    }
                }
            }
        }
        Self { rows }
    }

    /// The files that spell `name` as a literal, one line each.
    pub(super) fn naming(&self, name: &str) -> Vec<(String, u32)> {
        let mut named = self.rows.get(name).map_or_else(Vec::new, Clone::clone);
        named.sort();
        named
    }
}

/// The names the base version of a config defined (a compose service, a job, a
/// workflow, a tox env, an export subpath, as the key table reads them) that the
/// head no longer does: a reader elsewhere still spells what the change renamed,
/// whatever the name's length or vocabulary.
pub(super) fn dropped_definitions(state: &ScoredState, path: &str) -> Vec<String> {
    let file = state.root_dir.join(path);
    let source = state.run.source();
    let Ok(Some(base)) = source.base().read_lossy(&file) else {
        return Vec::new();
    };
    let kept =
        crate::keytable::defined_names(path, &source.read_to_string(&file).unwrap_or_default());
    crate::keytable::defined_names(path, &base)
        .into_iter()
        .filter(|name| !kept.contains(name))
        .collect()
}

/// The terms of the lines of `path` no hunk of the change wrote.
pub(super) fn untouched_terms(state: &ScoredState, path: &str) -> rustc_hash::FxHashSet<String> {
    let text = state
        .run
        .source()
        .read_to_string(&state.root_dir.join(path));
    let touched: Vec<(u32, u32)> = state
        .hunk_texts
        .iter()
        .filter(|h| h.path == path && h.new_len > 0)
        .map(crate::changed_lines::HunkText::new_range)
        .collect();
    (1..)
        .zip(text.as_deref().unwrap_or("").lines())
        .filter(|(n, l)| {
            !super::mentions::commented(l) && !touched.iter().any(|&(a, b)| a <= *n && *n <= b)
        })
        .flat_map(|(_, l)| terms(l))
        .collect()
}

/// The run's limits for a read of the snapshot's files.
pub(super) fn read_budget(state: &ScoredState) -> crate::snapshot::RunBudget {
    let (deadline, cancel) = state.run.io_limits();
    crate::snapshot::RunBudget {
        deadline,
        cancel: Some(cancel),
        read_fault: None,
    }
}

/// Records beside a new one that are read as one schema at most this many of.
const MAX_SCHEMA_SIBLINGS: usize = 16;

/// The keys every record beside a new one holds at its top level: a directory
/// of records one schema reads (`data/001-alpha.yaml`, `data/003-gamma.yaml`),
/// whose reader names those keys as fields and reads the new record too,
/// whether it spells them or lacks one (gh#743). Two siblings make a
/// collection; one is a pair, whose shared keys say nothing of a schema.
pub(super) fn schema_keys(state: &ScoredState, path: &str, tree: &[String]) -> Vec<String> {
    use crate::facts::config::{Dialect, Options, read};
    let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
    let Some((_, ext)) = name.rsplit_once('.') else {
        return Vec::new();
    };
    let dialect = match ext {
        "yaml" | "yml" => Dialect::Yaml,
        "json" => Dialect::Json,
        "toml" => Dialect::Toml,
        _ => return Vec::new(),
    };
    let changed: rustc_hash::FxHashSet<String> = super::changed_rels(state).collect();
    let siblings: Vec<&String> = tree
        .iter()
        .filter(|p| {
            let (d, n) = p.rsplit_once('/').unwrap_or(("", p.as_str()));
            d == dir && n.rsplit_once('.').is_some_and(|(_, e)| e == ext) && !changed.contains(*p)
        })
        .take(MAX_SCHEMA_SIBLINGS)
        .collect();
    if siblings.len() < 2 {
        return Vec::new();
    }
    let source = state.run.source();
    let budget = read_budget(state);
    let mut shared: Option<rustc_hash::FxHashSet<String>> = None;
    for sibling in siblings {
        let Some(text) = source.read_to_string(&state.root_dir.join(sibling.as_str())) else {
            return Vec::new();
        };
        let reading = read(dialect, text.as_bytes(), Options::default(), &budget);
        let [document] = reading.documents.as_slice() else {
            return Vec::new();
        };
        let keys: rustc_hash::FxHashSet<String> = document
            .entries()
            .iter()
            .filter(|e| !e.key.templated && key_like(&e.key.text))
            .map(|e| e.key.text.clone())
            .collect();
        shared = Some(match shared {
            Some(before) => before.intersection(&keys).cloned().collect(),
            None => keys,
        });
    }
    let Some(shared) = shared else {
        return Vec::new();
    };
    let mut keys: Vec<String> = shared.into_iter().collect();
    keys.sort();
    keys
}
