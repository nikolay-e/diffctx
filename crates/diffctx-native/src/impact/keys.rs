//! What a config line spells that a reader of the config may spell back, as text:
//! the words and the values a diff of a config touched, found the same way on the
//! changed side and on the reader's. The reader side is the token index's
//! (`Cells::spelled_lines`); this is the vocabulary both sides agree on.

use super::ScoredState;

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

pub(super) fn key_words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .map(|w| w.trim_matches('-'))
        .filter(|w| key_like(w))
        .map(str::to_string)
}

/// The key-shaped words a changed line offers to search. A value that is
/// spelled whole (an image, a path, an address: `phrase_like`) is searched whole
/// and gives no words of its own, as `1-debian13` of `app:2.14.1-debian13` is no
/// key (#707); a quoted sentence is prose and gives none either (#700). A reader's
/// line is read for every word (`key_words`): it is the needle that is narrow.
fn changed_words(text: &str) -> std::vec::IntoIter<String> {
    fn collect(text: &str, depth: usize, out: &mut Vec<String>) {
        for raw in split_tokens(text) {
            let token = trim_flow(raw);
            match token.chars().next() {
                Some(q @ ('\'' | '"')) if token.len() >= 2 && token.ends_with(q) => {
                    let inner = &token[1..token.len() - 1];
                    if depth < 2 {
                        collect(inner, depth + 1, out);
                    }
                }
                _ => match assignment(token) {
                    Some((head, value)) if depth < 2 => {
                        words(head, out);
                        collect(value, depth + 1, out);
                    }
                    _ if phrase_like(token) => {}
                    _ => words(token, out),
                },
            }
        }
    }
    fn words(text: &str, out: &mut Vec<String>) {
        out.extend(key_words(text));
    }
    let mut out = Vec::new();
    collect(&without_sentences(text), 0, &mut out);
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

const MIN_LITERAL_NAME: usize = 4;
const NAMES_PER_SEARCH: usize = 40;
/// A name that fills more files than this is a common word, not a reference.
const MAX_LITERAL_FILES: usize = 12;
const SQL_VERBS: &[&str] = &[
    "select", "insert", "update", "delete", "with", "call", "merge",
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

/// What a file's strings and values are read as.
#[derive(Clone, Copy)]
struct Reading {
    /// Source code: a string names a callee only as an argument (`getattr(m,
    /// "f")`, `@MethodSource("A#m")`) or as a qualified path in a list.
    code: bool,
    /// A config or a command file: its values and flags name what they run.
    values: bool,
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
        (code || values).then_some(Self { code, values })
    }
}

/// Whether `text` spells `name` as a literal in a file read as `reading`.
fn spells_literally(text: &str, name: &str, reading: Reading) -> bool {
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
            let head = &text[from..at];
            let tail = &text[end..to];
            let qualified = !head.is_empty();
            let lead = text[..from - 1].trim_end().chars().next_back();
            let placed = reading.values
                || matches!(lead, Some('(' | ','))
                || (qualified && matches!(lead, Some('[')));
            let reference = placed
                && !inside.contains(char::is_whitespace)
                && (!qualified || head.ends_with(qualifier))
                && (tail.is_empty() || tail.starts_with(['(', '=', ':', '[']));
            let query = sql
                && inside
                    .split_whitespace()
                    .next()
                    .is_some_and(|w| SQL_VERBS.contains(&w.to_lowercase().as_str()));
            return reference || query;
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
                && ["$", "%s", "#{", "{"].iter().any(|m| rest.starts_with(m))
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
        format!(r#"["'`]([^"'`[:space:]]*[.:#/\\])?({all})["'`(=:\[]"#),
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
        forms.push(format!(r#"({heads})(\$|%s|#\{{|\{{)"#));
    }
    forms.join("|")
}

impl LiteralNames {
    /// The lines of the tree at the range's head, outside the change, that spell
    /// one of `names` as a literal.
    pub(super) fn build(state: &ScoredState, diff_range: Option<&str>, names: &[&str]) -> Self {
        let mut wanted: Vec<&str> = names.iter().copied().filter(|n| literal_name(n)).collect();
        wanted.sort_unstable();
        wanted.dedup();
        if wanted.is_empty() {
            return Self::default();
        }
        let changed: rustc_hash::FxHashSet<String> = super::changed_rels(state).collect();
        let head = super::head_of(diff_range);
        let source = state.run.source();
        let mut rows: rustc_hash::FxHashMap<String, Vec<(String, u32)>> =
            rustc_hash::FxHashMap::default();
        for chunk in wanted.chunks(NAMES_PER_SEARCH) {
            if !state.run.check() {
                break;
            }
            let templates = format!(
                r"[.]({})[(]",
                chunk
                    .iter()
                    .map(|n| regex::escape(n))
                    .collect::<Vec<_>>()
                    .join("|")
            );
            let found = [
                (literal_pattern(chunk), Vec::new()),
                (templates, TEMPLATE_GLOBS.to_vec()),
            ];
            for (pattern, pathspec) in found {
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
                    if changed.contains(path)
                        || super::is_document(path)
                        || super::mentions::selects_paths(path)
                        || super::mentions::commented(text)
                        || matches!(
                            super::extension(path).as_str(),
                            "diff"
                                | "patch"
                                | "lock"
                                | "tex"
                                | "bib"
                                | "csv"
                                | "tsv"
                                | "log"
                                | "svg"
                        )
                    {
                        continue;
                    }
                    let Some(reading) = Reading::of(path) else {
                        continue;
                    };
                    for name in chunk.iter().filter(|n| spells_literally(text, n, reading)) {
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

    /// The files that spell `name` as a literal, one line each; none when the
    /// name is a common word.
    pub(super) fn naming(&self, name: &str) -> Vec<(String, u32)> {
        let Some(files) = self.rows.get(name) else {
            return Vec::new();
        };
        let mut named = files.clone();
        if named.len() > MAX_LITERAL_FILES {
            return Vec::new();
        }
        named.sort();
        named
    }
}
