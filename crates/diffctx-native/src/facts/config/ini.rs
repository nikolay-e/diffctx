//! INI, as Python's `configparser` reads it (the dialect `setup.cfg`, `tox.ini`
//! and `pytest.ini` are written in; no library of ours reads it).
//!
//! - `[name]` opens a section; the name is everything between the first `[` and
//!   the last `]`, whitespace included.
//! - `key = value` or `key: value`: the first `=` or `:` of the line is the
//!   delimiter, so a value may hold either; key and value are stripped. Keys
//!   keep their case (`configparser` lowercases them).
//! - A line that has no delimiter is a key without a value (a null).
//! - A line whose first non-blank character is `#` or `;` is a comment; there are
//!   no inline comments.
//! - A line indented deeper than its key continues the value, with a line break
//!   between; blank lines inside a value are kept when a continuation follows.
//! - Keys before the first section sit in the document's own map. A section
//!   that stands twice, or a key that does, is two entries.
//!
//! The document is a map of its keys and its sections; a section is a map of its
//! keys, every value a string.

use crate::snapshot::RunBudget;

use super::Reading;
use super::syntax::{self, Guard, LineIndex};
use super::tree::{Entry, Key, Node, Scalar, ScalarKind, Value};

struct Pending {
    key: Key,
    value: String,
    start: usize,
    end: usize,
    indent: usize,
    blanks: usize,
    delimited: bool,
}

struct Open {
    key: Key,
    start: usize,
    end: usize,
    entries: Vec<Entry>,
}

pub(super) fn read(text: &str, budget: &RunBudget, out: &mut Reading) {
    let lines = LineIndex::new(text);
    let mut guard = Guard::new(budget);
    let mut root: Vec<Entry> = Vec::new();
    let mut section: Option<Open> = None;
    let mut pending: Option<Pending> = None;
    let mut errors = Vec::new();
    let mut offset = 0;
    for (number, raw) in text.split_inclusive('\n').enumerate() {
        let start = offset;
        offset += raw.len();
        if !guard.poll(number) {
            break;
        }
        let line = raw.trim_end_matches(['\n', '\r']);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if let Some(held) = pending.as_mut() {
                held.blanks += 1;
            }
            continue;
        }
        if trimmed.starts_with(['#', ';']) {
            continue;
        }
        let lead = line.len() - line.trim_start().len();
        if let Some(held) = pending.as_mut().filter(|held| lead > held.indent) {
            held.value.push_str(&"\n".repeat(held.blanks + 1));
            held.value.push_str(trimmed);
            held.blanks = 0;
            held.end = start + lead + trimmed.len();
            continue;
        }
        flush(&mut pending, &mut section, &mut root, &lines);
        if trimmed.starts_with('[') {
            let body = &line[lead..];
            match body.rfind(']') {
                Some(close) => {
                    close_section(&mut section, &mut root, &lines);
                    let name_start = start + lead + 1;
                    section = Some(Open {
                        key: Key {
                            text: body[1..close].to_string(),
                            span: lines.span(name_start, name_start + close - 1),
                            templated: false,
                        },
                        start: start + lead,
                        end: start + lead + close + 1,
                        entries: Vec::new(),
                    });
                }
                None => errors.push((lines.line_of(start as u32), lines.line_of(start as u32))),
            }
            continue;
        }
        let body = &line[lead..];
        let key_start = start + lead;
        let (key, value, value_start, delimited) = match body.find(['=', ':']) {
            Some(at) => {
                let rest = &body[at + 1..];
                let gap = rest.len() - rest.trim_start().len();
                (
                    body[..at].trim_end(),
                    rest.trim(),
                    key_start + at + 1 + gap,
                    true,
                )
            }
            None => (
                body.trim_end(),
                "",
                key_start + body.trim_end().len(),
                false,
            ),
        };
        pending = Some(Pending {
            key: Key {
                text: key.to_string(),
                span: lines.span(key_start, key_start + key.len()),
                templated: false,
            },
            value: value.to_string(),
            start: value_start,
            end: value_start + value.len(),
            indent: lead,
            blanks: 0,
            delimited,
        });
    }
    flush(&mut pending, &mut section, &mut root, &lines);
    close_section(&mut section, &mut root, &lines);
    out.documents = vec![Node {
        span: lines.span(0, text.len()),
        value: Value::Map(root),
        templated: false,
    }];
    out.errors = syntax::merge_ranges(errors);
    out.flaws.extend(guard.flaws);
}

/// Puts the value that was being read into the open section, or the document.
fn flush(
    pending: &mut Option<Pending>,
    section: &mut Option<Open>,
    root: &mut Vec<Entry>,
    lines: &LineIndex,
) {
    let Some(held) = pending.take() else {
        return;
    };
    let span = lines.span(held.start, held.end);
    let value = if held.delimited {
        Node {
            span,
            value: Value::Scalar(Scalar {
                text: held.value,
                kind: ScalarKind::Str,
                quoted: false,
            }),
            templated: false,
        }
    } else {
        Node::null(span)
    };
    let entry = Entry {
        key: held.key,
        value,
    };
    match section {
        Some(open) => {
            open.end = held.end.max(open.end);
            open.entries.push(entry);
        }
        None => root.push(entry),
    }
}

fn close_section(section: &mut Option<Open>, root: &mut Vec<Entry>, lines: &LineIndex) {
    if let Some(open) = section.take() {
        root.push(Entry {
            key: open.key,
            value: Node {
                span: lines.span(open.start, open.end),
                value: Value::Map(open.entries),
                templated: false,
            },
        });
    }
}
