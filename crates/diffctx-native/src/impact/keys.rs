//! What a config line spells that a reader of the config may spell back, as text:
//! the words and the values a diff of a config touched, found the same way on the
//! changed side and on the reader's. The reader side is the token index's
//! (`Cells::spelled_lines`); this is the vocabulary both sides agree on.

/// The terms of one line of a config: its key-shaped words and its value phrases.
pub(super) fn terms(line: &str) -> Vec<String> {
    key_words(line).chain(value_phrases(line)).collect()
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
                _ => token.split_once('=').and_then(|(head, value)| {
                    let key = !head.is_empty()
                        && head
                            .chars()
                            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-'));
                    key.then_some(value)
                }),
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
