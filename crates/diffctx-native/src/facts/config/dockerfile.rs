//! A Dockerfile as instructions with spans: the text the BuildKit frontend reads,
//! without reading what the shell of a `RUN` will.
//!
//! An instruction runs over its physical lines while each ends in the escape
//! character (a backslash, or the one `# escape=` names); comment lines inside it
//! are skipped. A heredoc (`RUN <<EOF`, `COPY <<EOF /dest`) takes the lines up to
//! its terminator into the instruction, unread. The JSON form of a list
//! (`COPY ["a b", "c"]`) is read as its strings. Nothing runs and nothing is
//! expanded: a word is the text as written, with where it is.

use crate::facts::Span;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Word {
    pub text: String,
    pub span: Span,
}

/// `--from=build` is the flag `from` with the value `build`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Flag {
    pub name: String,
    pub value: String,
    pub span: Span,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Instruction {
    /// Upper-cased: instruction names are case-insensitive.
    pub keyword: String,
    pub keyword_span: Span,
    pub flags: Vec<Flag>,
    pub words: Vec<Word>,
    /// The instruction's lines, a heredoc's body included.
    pub span: Span,
    pub heredoc: bool,
}

impl Instruction {
    pub fn flag(&self, name: &str) -> Option<&Flag> {
        self.flags.iter().find(|f| f.name == name)
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Dockerfile {
    pub instructions: Vec<Instruction>,
}

struct Line<'a> {
    text: &'a str,
    start: usize,
    number: u32,
}

fn lines(text: &str) -> Vec<Line<'_>> {
    let mut found = Vec::new();
    let mut start = 0;
    for (i, piece) in text.split_inclusive('\n').enumerate() {
        found.push(Line {
            text: piece.trim_end_matches(['\n', '\r']),
            start,
            number: i as u32 + 1,
        });
        start += piece.len();
    }
    found
}

fn span(start: usize, end: usize, from: u32, to: u32) -> Span {
    Span {
        bytes: start as u32..end as u32,
        lines: (from, to),
    }
}

/// The escape character a leading `# escape=` directive names.
fn escape_of(lines: &[Line<'_>]) -> char {
    for line in lines {
        let trimmed = line.text.trim();
        let Some(rest) = trimmed.strip_prefix('#') else {
            break;
        };
        let Some((key, value)) = rest.split_once('=') else {
            break;
        };
        if key.trim().eq_ignore_ascii_case("escape") {
            return value.trim().chars().next().unwrap_or('\\');
        }
    }
    '\\'
}

/// The whitespace-separated words of a physical line, with their bytes.
fn words_of(line: &Line<'_>, from: usize, to: usize) -> Vec<Word> {
    let text = &line.text[from..to];
    let mut found = Vec::new();
    let mut at = 0;
    let bytes = text.as_bytes();
    while at < bytes.len() {
        if bytes[at].is_ascii_whitespace() {
            at += 1;
            continue;
        }
        let begin = at;
        let mut quote: Option<u8> = None;
        while at < bytes.len() {
            let c = bytes[at];
            match quote {
                Some(q) if c == q => quote = None,
                Some(_) => {}
                None if c == b'"' || c == b'\'' => quote = Some(c),
                None if c.is_ascii_whitespace() => break,
                None => {}
            }
            at += 1;
        }
        found.push(Word {
            text: text[begin..at].to_string(),
            span: span(
                line.start + from + begin,
                line.start + from + at,
                line.number,
                line.number,
            ),
        });
    }
    found
}

fn unquote(word: &str) -> String {
    let trimmed = word.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = trimmed
            .strip_prefix(quote)
            .and_then(|r| r.strip_suffix(quote))
        {
            return inner.to_string();
        }
    }
    trimmed.to_string()
}

/// A heredoc terminator a word opens: `<<EOF`, `<<-EOF`, `<<"EOF"`.
fn terminator(word: &str) -> Option<(String, bool)> {
    let rest = word.strip_prefix("<<")?;
    if rest.starts_with('<') {
        return None;
    }
    let (rest, strip) = match rest.strip_prefix('-') {
        Some(r) => (r, true),
        None => (rest, false),
    };
    let name = rest.trim_matches(|c| c == '"' || c == '\'');
    (!name.is_empty()).then(|| (name.to_string(), strip))
}

pub fn parse(text: &str) -> Dockerfile {
    let all = lines(text);
    let escape = escape_of(&all);
    let mut instructions = Vec::new();
    let mut i = 0;
    while i < all.len() {
        let first = &all[i];
        let trimmed = first.text.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }
        let begin = first.start + (first.text.len() - trimmed.len());
        let mut words: Vec<Word> = Vec::new();
        let mut last;
        let mut at = i;
        loop {
            let line = &all[at];
            let content = line.text.trim_end();
            let continued = content.ends_with(escape);
            let end = if continued {
                content.len() - escape.len_utf8()
            } else {
                content.len()
            };
            words.extend(words_of(line, 0, end));
            last = at;
            if !continued {
                break;
            }
            at += 1;
            // Comments and blank lines inside a continuation are skipped.
            while at < all.len() {
                let t = all[at].text.trim();
                if t.is_empty() || t.starts_with('#') {
                    at += 1;
                } else {
                    break;
                }
            }
            if at >= all.len() {
                break;
            }
        }
        let mut heredoc = false;
        let mut end_line = last;
        let opened: Vec<(String, bool)> = words
            .iter()
            .skip(1)
            .filter_map(|w| terminator(&w.text))
            .collect();
        if !opened.is_empty() {
            heredoc = true;
            let mut cursor = last + 1;
            for (name, strip) in &opened {
                while cursor < all.len() {
                    let body = all[cursor].text;
                    let body = if *strip {
                        body.trim_start_matches('\t')
                    } else {
                        body
                    };
                    let done = body.trim_end() == name;
                    end_line = cursor;
                    cursor += 1;
                    if done {
                        break;
                    }
                }
            }
        }
        i = end_line + 1;
        let end_byte = all[end_line].start + all[end_line].text.trim_end().len();
        let Some(head) = words.first().cloned() else {
            continue;
        };
        let mut rest: Vec<Word> = words.into_iter().skip(1).collect();
        let mut flags = Vec::new();
        while rest.first().is_some_and(|w| w.text.starts_with("--")) {
            let word = rest.remove(0);
            let body = &word.text[2..];
            let (name, value) = body.split_once('=').unwrap_or((body, ""));
            flags.push(Flag {
                name: name.to_string(),
                value: unquote(value),
                span: word.span,
            });
        }
        // The JSON form of a list.
        if rest.first().is_some_and(|w| w.text.starts_with('[')) {
            let joined: Vec<&str> = rest.iter().map(|w| w.text.as_str()).collect();
            if let Ok(items) = serde_json::from_str::<Vec<String>>(&joined.join(" ")) {
                let whole = span(
                    rest[0].span.bytes.start as usize,
                    rest[rest.len() - 1].span.bytes.end as usize,
                    rest[0].span.lines.0,
                    rest[rest.len() - 1].span.lines.1,
                );
                rest = items
                    .into_iter()
                    .map(|text| Word {
                        text,
                        span: whole.clone(),
                    })
                    .collect();
            }
        }
        instructions.push(Instruction {
            keyword: head.text.to_ascii_uppercase(),
            keyword_span: head.span,
            flags,
            words: rest,
            span: span(begin, end_byte, first.number, all[end_line].number),
            heredoc,
        });
    }
    Dockerfile { instructions }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_run_over_continuations_and_skip_comments_inside_them() {
        let parsed = parse("FROM alpine AS b\nRUN a \\\n  # note\n  && b\n# c\nCOPY x y\n");
        let keys: Vec<&str> = parsed
            .instructions
            .iter()
            .map(|i| i.keyword.as_str())
            .collect();
        assert_eq!(keys, ["FROM", "RUN", "COPY"]);
        let run = &parsed.instructions[1];
        assert_eq!(run.span.lines, (2, 4));
        assert_eq!(
            run.words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>(),
            ["a", "&&", "b"]
        );
    }

    #[test]
    fn flags_and_the_json_form_are_read() {
        let parsed = parse("COPY --from=build --chown=1:1 [\"a b\", \"c\"] /d\n");
        let copy = &parsed.instructions[0];
        assert_eq!(copy.flag("from").map(|f| f.value.as_str()), Some("build"));
        assert_eq!(copy.flags.len(), 2);
    }

    #[test]
    fn a_heredoc_takes_its_body_unread() {
        let parsed = parse("RUN <<EOF\necho hi\nEOF\nFROM x\n");
        assert_eq!(parsed.instructions.len(), 2);
        assert!(parsed.instructions[0].heredoc);
        assert_eq!(parsed.instructions[0].span.lines, (1, 3));
    }

    #[test]
    fn a_named_escape_character_continues_lines() {
        let parsed = parse("# escape=`\nRUN a `\n b\n");
        assert_eq!(parsed.instructions.len(), 1);
        assert_eq!(parsed.instructions[0].words.len(), 2);
    }
}
