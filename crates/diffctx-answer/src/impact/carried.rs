//! Code a program carries in a string to a shell or an interpreter, read through
//! the string literals of its tree: the whole literal, in whatever layout, never
//! the one line a search hit.

use std::ops::Range;

use once_cell::sync::Lazy;
use regex::Regex;

use super::ScoredState;
use super::keys::{called, word_char};
use crate::decide::cells::Cells;

/// The lines that hand code to an interpreter: the flag as an argument of its own
/// (`"-c"`), or `python -c` spelled inside a shell string.
static CARRIER_LINES: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"["'](-c|-e|-r|-p|--eval|--command)["']|(python[0-9.]*|nodejs|node|ruby|perl|php)[[:space:]]+(-[cer]|--eval)[[:space:]]"#,
    )
    .expect("a constant pattern is valid")
});

/// `python -c ` in a string: what follows is the code.
static SPELLED: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?:^|[^\w./-])(?:python[0-9.]*|nodejs|node|ruby|perl|php)\s+(?:-[cer]|--eval)\s")
        .expect("a constant pattern is valid")
});

const FLAGS: [&str; 6] = ["-c", "-e", "-r", "-p", "--eval", "--command"];

/// A head file's text and the byte ranges of its string literals, whole: a piece
/// keeps its delimiters.
pub(super) struct Strings {
    text: String,
    pieces: Vec<Range<usize>>,
}

impl Strings {
    /// `None` when no tree read the file's strings: the line is all there is.
    pub(super) fn of(state: &ScoredState, cells: &Cells, path: &str) -> Option<Self> {
        let read = cells.string_pieces(path).unwrap_or_else(|e| {
            tracing::warn!(
                "impact: the strings of {path} were not read ({e}); its answer is unknown"
            );
            state
                .run
                .note(diffctx_snapshot::resource::LimitReason::ReadFailed);
            None
        });
        let (text, ranges) = read?;
        Some(Self {
            text,
            pieces: ranges
                .into_iter()
                .map(|range| range.start as usize..range.end as usize)
                .collect(),
        })
    }

    fn line_of(&self, at: usize) -> u32 {
        1 + self.text.as_bytes()[..at]
            .iter()
            .filter(|&&b| b == b'\n')
            .count() as u32
    }

    /// What piece `at` holds, without its prefix and delimiters, so the first
    /// word of a command string stays in command position.
    fn content(&self, at: usize) -> &str {
        let raw = &self.text[self.pieces[at].clone()];
        let opened = raw.trim_start_matches(['r', 'R', 'b', 'B', 'u', 'U', 'f', 'F']);
        let body = if opened.starts_with(['"', '\'', '`', '#']) {
            opened.trim_start_matches(['"', '\'', '`', '#'])
        } else {
            raw
        };
        body.trim_end_matches(['"', '\'', '`', '#'])
    }

    fn covering(&self, line: u32) -> impl Iterator<Item = usize> + '_ {
        (0..self.pieces.len()).filter(move |&at| {
            let range = &self.pieces[at];
            self.line_of(range.start) <= line && line <= self.line_of(range.end.saturating_sub(1))
        })
    }

    /// The strings that span `line`, each whole.
    pub(super) fn at(&self, line: u32) -> Vec<String> {
        self.covering(line)
            .map(|at| self.content(at).to_string())
            .collect()
    }

    /// The code the strings on `line` hand to an interpreter.
    fn code_at(&self, line: u32) -> Vec<String> {
        let mut code = Vec::new();
        for at in self.covering(line) {
            let content = self.content(at);
            if let Some(spelled) = SPELLED.find(content) {
                code.push(content[spelled.end()..].to_string());
            }
            if FLAGS.contains(&content) {
                code.extend(self.argument_after(at));
            }
        }
        code
    }

    /// The string handed after the flag in piece `flag`: the next literal, or the
    /// one a name the argument spells is assigned.
    fn argument_after(&self, flag: usize) -> Option<String> {
        let between = |to: usize| &self.text[self.pieces[flag].end..self.pieces[to].start];
        let separator = |c: char| c.is_whitespace() || c == ',';
        if flag + 1 < self.pieces.len() && between(flag + 1).chars().all(separator) {
            return Some(self.content(flag + 1).to_string());
        }
        let rest = self.text[self.pieces[flag].end..].trim_start_matches(separator);
        let name: String = rest.chars().take_while(|&c| word_char(c)).collect();
        if name.is_empty() {
            return None;
        }
        (0..self.pieces.len())
            .find(|&at| {
                self.text[..self.pieces[at].start]
                    .trim_end()
                    .strip_suffix('=')
                    .map(|head| head.trim_end().trim_end_matches(':'))
                    .and_then(|head| head.strip_suffix(name.as_str()))
                    .is_some_and(|head| !head.ends_with(word_char))
            })
            .map(|at| self.content(at).to_string())
    }
}

/// Whether `code` calls `name`, or imports it.
fn mentions(code: &str, name: &str) -> bool {
    called(code, name)
        || code.lines().flat_map(|line| line.split(';')).any(|part| {
            let part = part.trim_start();
            (part.starts_with("import ") || part.starts_with("from "))
                && part.match_indices(name).any(|(at, _)| {
                    !part[..at].ends_with(word_char)
                        && !part[at + name.len()..].starts_with(word_char)
                })
        })
}

/// The files that hand a string to an interpreter and name one of `names` in it,
/// as `(name, path, line)`: the string is another language's code, which no
/// reader here parses, so what it names is only ever a candidate. A file that
/// names none of them in any line carries none of their code, so the candidates
/// are the files the run's shared search of the names already found, and the
/// carrying lines are read off their text.
pub(super) fn names_in_carried_code(
    state: &ScoredState,
    cells: &Cells,
    head: Option<&str>,
    names: &[&str],
) -> Vec<(String, String, u32)> {
    if names.is_empty() {
        return Vec::new();
    }
    let needles: Vec<String> = names.iter().map(|name| (*name).to_string()).collect();
    let namers = super::removed::read_or_note(
        state,
        diffctx_snapshot::grep::grep_lines(&state.root_dir, &state.run.source(), &needles, head),
    );
    let mut candidates: Vec<&str> = namers.rows.iter().map(|(path, ..)| path.as_str()).collect();
    candidates.sort_unstable();
    candidates.dedup();
    let mut found = Vec::new();
    for path in state.run.bounded(candidates) {
        if super::callers::is_document(path) {
            continue;
        }
        let Some(text) = cells.head_text(state, path) else {
            continue;
        };
        let carrying: Vec<(u32, &str)> = (1..)
            .zip(text.lines())
            .filter(|(_, line)| CARRIER_LINES.is_match(line))
            .collect();
        if carrying.is_empty() {
            continue;
        }
        let Some(strings) = Strings::of(state, cells, path) else {
            continue;
        };
        for (line, text) in carrying {
            if super::mentions::commented_in(path, text) {
                continue;
            }
            for code in strings.code_at(line) {
                for name in names.iter().filter(|name| mentions(&code, name)) {
                    found.push(((*name).to_string(), path.to_string(), line));
                }
            }
        }
    }
    found
}
