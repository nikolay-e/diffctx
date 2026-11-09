//! Go templates (Helm charts, Helmfile `.gotmpl`) read around, never expanded.
//!
//! A template is not data: `{{- if .Values.x }}` on a line of its own is no YAML.
//! [`mask`] writes the file again with every `{{ ... }}` action (the delimiters,
//! the quoted and raw strings and the `/* */` comment of the `text/template`
//! syntax) replaced by bytes of the same length, so the YAML grammar reads the
//! rest at the byte offsets and lines of the original:
//!
//! - an action alone on its lines (a control action, an `include ... | nindent`)
//!   becomes blanks, as a blank line is valid wherever the action stood;
//! - an action inside a line becomes `_` of the same length, a plain scalar the
//!   grammar can read, and the scalar it touches is marked `templated`.
//!
//! The actions are kept as [`Template`] facts: their text and where they stood.

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::facts::Span;

use super::syntax::LineIndex;

/// One action of a template, as written.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Template {
    pub span: Span,
    /// Alone on its lines: it stands for whole lines of the rendered file (a
    /// block of keys, nothing) and not for a part of a value.
    pub standalone: bool,
    /// `{{ ... }}` with its delimiters.
    pub text: String,
}

pub(super) struct Masked {
    pub(super) text: String,
    pub(super) regions: Vec<Template>,
}

/// The file with its actions masked, and the actions.
pub(super) fn mask(text: &str) -> Masked {
    let actions = actions(text.as_bytes());
    let lines = LineIndex::new(text);
    let mut bytes = text.as_bytes().to_vec();
    let mut regions = Vec::with_capacity(actions.len());
    for (at, &(start, end)) in actions.iter().enumerate() {
        let standalone = is_standalone(text, &actions, at);
        let filler = if standalone { b' ' } else { b'_' };
        for byte in &mut bytes[start..end] {
            if *byte != b'\n' {
                *byte = filler;
            }
        }
        regions.push(Template {
            span: lines.span(start, end),
            standalone,
            text: text[start..end].to_string(),
        });
    }
    Masked {
        // Every action starts and ends on an ASCII delimiter and is replaced
        // byte for byte by ASCII, so the text is still UTF-8.
        text: String::from_utf8_lossy(&bytes).into_owned(),
        regions,
    }
}

/// Whether a region of a file lies in an action; `regions` are in source order.
pub(super) fn touches(regions: &[Template], bytes: &Range<u32>) -> bool {
    let first = regions.partition_point(|t| t.span.bytes.end <= bytes.start);
    regions
        .get(first)
        .is_some_and(|t| t.span.bytes.start < bytes.end.max(bytes.start + 1))
}

/// `[start, end)` of every action, in order. An action that never closes is not
/// one: the rest of the file is text.
fn actions(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut at = 0;
    while at + 1 < bytes.len() {
        if bytes[at] == b'{' && bytes[at + 1] == b'{' {
            match action_end(bytes, at + 2) {
                Some(end) => {
                    found.push((at, end));
                    at = end;
                }
                None => break,
            }
        } else {
            at += 1;
        }
    }
    found
}

/// The offset after the `}}` that closes an action whose body starts at `from`.
fn action_end(bytes: &[u8], from: usize) -> Option<usize> {
    let mut at = from;
    // `{{- /* ... */ -}}`: the comment ends at `*/`, which may hold `}}`.
    let mut body = at;
    if bytes.get(body) == Some(&b'-') && bytes.get(body + 1).is_some_and(u8::is_ascii_whitespace) {
        body += 2;
    }
    while bytes.get(body).is_some_and(u8::is_ascii_whitespace) {
        body += 1;
    }
    if bytes[body.min(bytes.len())..].starts_with(b"/*") {
        at = body + 2;
        while at + 1 < bytes.len() && !(bytes[at] == b'*' && bytes[at + 1] == b'/') {
            at += 1;
        }
        at += 2;
    }
    while at + 1 < bytes.len() {
        match bytes[at] {
            quote @ (b'"' | b'\'') => {
                // A quoted string or character does not run past its line.
                at += 1;
                while at < bytes.len() && bytes[at] != quote && bytes[at] != b'\n' {
                    at += if bytes[at] == b'\\' { 2 } else { 1 };
                }
                at += 1;
            }
            b'`' => {
                at += 1;
                while at < bytes.len() && bytes[at] != b'`' {
                    at += 1;
                }
                at += 1;
            }
            b'}' if bytes[at + 1] == b'}' => return Some(at + 2),
            _ => at += 1,
        }
    }
    None
}

/// Whether the action `at`, with the actions that share its lines, is all that
/// is on them.
fn is_standalone(text: &str, actions: &[(usize, usize)], at: usize) -> bool {
    let blank = |from: usize, to: usize| text[from..to].trim().is_empty();
    let mut start = actions[at].0;
    let mut index = at;
    loop {
        let line_start = text[..start].rfind('\n').map_or(0, |p| p + 1);
        if blank(line_start, start) {
            break;
        }
        if index == 0 {
            return false;
        }
        let (before_start, before_end) = actions[index - 1];
        if before_end > start || !blank(before_end, start) {
            return false;
        }
        start = before_start;
        index -= 1;
    }
    let mut end = actions[at].1;
    let mut index = at;
    loop {
        let line_end = text[end..].find('\n').map_or(text.len(), |p| end + p);
        if blank(end, line_end) {
            return true;
        }
        let Some(&(next_start, next_end)) = actions.get(index + 1) else {
            return false;
        };
        if next_start < end || !blank(end, next_start) {
            return false;
        }
        end = next_end;
        index += 1;
    }
}
