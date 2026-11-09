//! TOML read with the `toml` crate's own spanned parse, which recovers from an
//! error and says where it was. Its tables keep no source order (the crate's
//! `preserve_order` feature is off), so entries are put back in the order their
//! keys stand in the file.

use toml::Spanned;
use toml::de::{DeTable, DeValue};

use crate::snapshot::RunBudget;

use super::Reading;
use super::syntax::{self, Guard, LineIndex};
use super::tree::{Entry, Key, Node, Scalar, ScalarKind, Value};

pub(super) fn read(text: &str, budget: &RunBudget, out: &mut Reading) {
    let lines = LineIndex::new(text);
    let (document, errors) = DeTable::parse_recoverable(text);
    let mut ranges = Vec::new();
    for error in &errors {
        ranges.push(match error.span() {
            Some(span) => (
                lines.line_of(span.start as u32),
                lines.line_of(span.end.saturating_sub(1).max(span.start) as u32),
            ),
            None => (1, lines.count()),
        });
    }
    out.errors = syntax::merge_ranges(ranges);
    let mut reader = Reader {
        lines: &lines,
        guard: Guard::new(budget),
    };
    let root = Node {
        span: lines.span(0, text.len()),
        value: Value::Map(reader.entries(document.get_ref(), 0)),
        templated: false,
    };
    out.documents = vec![root];
    out.flaws.extend(reader.guard.flaws);
}

struct Reader<'a> {
    lines: &'a LineIndex,
    guard: Guard<'a>,
}

impl Reader<'_> {
    fn entries(&mut self, table: &DeTable<'_>, depth: usize) -> Vec<Entry> {
        let mut entries: Vec<Entry> = table
            .iter()
            .map(|(key, value)| Entry {
                key: Key {
                    text: key.get_ref().to_string(),
                    span: self.lines.span(key.span().start, key.span().end),
                    templated: false,
                },
                value: self.value(value, depth + 1),
            })
            .collect();
        entries.sort_by_key(|entry| entry.key.span.bytes.start);
        entries
    }

    fn value(&mut self, value: &Spanned<DeValue<'_>>, depth: usize) -> Node {
        let span = self.lines.span(value.span().start, value.span().end);
        if !self.guard.enter(depth) {
            return Node::null(span);
        }
        let scalar = |text: String, kind: ScalarKind, quoted: bool| {
            Value::Scalar(Scalar { text, kind, quoted })
        };
        let value = match value.get_ref() {
            DeValue::String(text) => scalar(text.to_string(), ScalarKind::Str, true),
            DeValue::Integer(number) => scalar(number.to_string(), ScalarKind::Int, false),
            DeValue::Float(number) => scalar(number.to_string(), ScalarKind::Float, false),
            DeValue::Boolean(flag) => scalar(flag.to_string(), ScalarKind::Bool, false),
            DeValue::Datetime(moment) => scalar(moment.to_string(), ScalarKind::Datetime, false),
            DeValue::Array(items) => Value::Seq(
                items
                    .iter()
                    .map(|item| self.value(item, depth + 1))
                    .collect(),
            ),
            DeValue::Table(table) => Value::Map(self.entries(table, depth)),
        };
        Node {
            span,
            value,
            templated: false,
        }
    }
}
