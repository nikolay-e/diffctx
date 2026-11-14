//! YAML 1.2 read with the `tree-sitter-yaml` grammar, which keeps what the
//! readers of a value do not: the span of every node. Scalars are decoded here
//! (quotes, escapes, line folding, block scalars with their chomping and
//! indentation indicators), plain scalars are typed by the grammar (the core
//! schema), anchors are resolved by copying the anchored node (with its own
//! spans), and a `<<` merge key stays an entry that [`Node::get`] looks through.

use rustc_hash::FxHashMap;
use tree_sitter::{Language as TsLanguage, Node as TsNode};

use crate::facts::Span;
use crate::snapshot::RunBudget;

use super::syntax::{self, Guard};
use super::template::{self, Template};
use super::tree::{Entry, Key, Node, Scalar, ScalarKind, Value};
use super::{Flaw, Options, Reading, Templating};

#[cfg(feature = "tree-sitter-yaml")]
fn grammar() -> Option<TsLanguage> {
    Some(TsLanguage::new(tree_sitter_yaml::LANGUAGE))
}

#[cfg(not(feature = "tree-sitter-yaml"))]
fn grammar() -> Option<TsLanguage> {
    None
}

pub(super) fn read(text: &str, options: Options, budget: &RunBudget, out: &mut Reading) {
    let (masked, templates) = match options.templating {
        Templating::Go => {
            let masked = template::mask(text);
            (Some(masked.text), masked.regions)
        }
        Templating::None => (None, Vec::new()),
    };
    let source = masked.as_deref().unwrap_or(text);
    let Some(grammar) = grammar() else {
        out.flaws.push(Flaw::Unsupported);
        return;
    };
    let Some(tree) = syntax::parse(&grammar, source.as_bytes(), budget) else {
        out.flaws.push(if crate::facts::expired(budget) {
            Flaw::Deadline
        } else {
            Flaw::Unsupported
        });
        out.templates = templates;
        return;
    };
    let root = tree.root_node();
    if root.has_error() {
        out.errors = syntax::error_lines(root);
    }
    let flaws;
    {
        let mut reader = Reader {
            text: source,
            templates: &templates,
            anchors: FxHashMap::default(),
            guard: Guard::new(budget),
        };
        out.documents = reader.documents(root);
        flaws = reader.guard.flaws;
    }
    let aliases: Vec<(u32, u32)> = flaws
        .iter()
        .filter_map(|flaw| match flaw {
            Flaw::UnresolvedAlias { line, .. } => Some((*line, *line)),
            _ => None,
        })
        .collect();
    if !aliases.is_empty() {
        out.errors.extend(aliases);
        out.errors = syntax::merge_ranges(std::mem::take(&mut out.errors));
    }
    out.flaws.extend(flaws);
    out.templates = templates;
}

struct Reader<'a> {
    text: &'a str,
    templates: &'a [Template],
    anchors: FxHashMap<String, Node>,
    guard: Guard<'a>,
}

impl Reader<'_> {
    fn documents(&mut self, root: TsNode) -> Vec<Node> {
        let mut documents = Vec::new();
        for document in syntax::children(root) {
            if document.kind() != "document" {
                continue;
            }
            let content = syntax::children(document)
                .into_iter()
                .find(|child| matches!(child.kind(), "block_node" | "flow_node"));
            documents.push(match content {
                Some(content) => self.node(content, -1, 0),
                None => Node::null(syntax::span(document)),
            });
        }
        documents
    }

    /// The text of a node as written. Tree-sitter offsets of a UTF-8 text are
    /// char boundaries, so the slice always exists.
    fn slice(&self, node: TsNode) -> &str {
        self.text
            .get(node.start_byte()..node.end_byte())
            .unwrap_or("")
    }

    fn touched(&self, span: &Span) -> bool {
        template::touches(self.templates, &span.bytes)
    }

    fn scalar(&self, at: TsNode, text: String, kind: ScalarKind, quoted: bool) -> Node {
        let span = syntax::span(at);
        Node {
            templated: self.touched(&span),
            span,
            value: Value::Scalar(Scalar { text, kind, quoted }),
        }
    }

    /// A `block_node` or `flow_node`: its anchor and tag, and the content they
    /// are of. `indent` is the column of the map entry or sequence item the
    /// node is the value of, `-1` for a document's own node.
    fn node(&mut self, wrapper: TsNode, indent: i64, depth: usize) -> Node {
        if !self.guard.enter(depth) {
            return Node::null(syntax::span(wrapper));
        }
        let mut anchor = None;
        let mut tag = None;
        let mut content = None;
        if matches!(wrapper.kind(), "block_node" | "flow_node") {
            for child in syntax::children(wrapper) {
                match child.kind() {
                    "anchor" => anchor = Some(self.anchor_name(child)),
                    "tag" => tag = Some(self.slice(child).to_string()),
                    _ if content.is_none() => content = Some(child),
                    _ => {}
                }
            }
        } else {
            content = Some(wrapper);
        }
        let mut node = match content {
            Some(content) => self.content(content, indent, depth),
            None => Node::null(syntax::span(wrapper)),
        };
        if tag.as_deref() == Some("!!str") {
            node = self.stringified(node);
        }
        if let Some(name) = anchor {
            self.anchors.insert(name, node.clone());
        }
        node
    }

    /// `!!str 80`: a scalar the tag makes a string, `!!str` alone the empty one.
    fn stringified(&self, node: Node) -> Node {
        let Node {
            span,
            value,
            templated,
        } = node;
        let value = match value {
            Value::Scalar(scalar) => Value::Scalar(Scalar {
                kind: ScalarKind::Str,
                ..scalar
            }),
            Value::Null => Value::Scalar(Scalar {
                text: String::new(),
                kind: ScalarKind::Str,
                quoted: false,
            }),
            collection @ (Value::Seq(_) | Value::Map(_)) => collection,
        };
        Node {
            span,
            value,
            templated,
        }
    }

    fn anchor_name(&self, node: TsNode) -> String {
        let named = syntax::children(node)
            .into_iter()
            .find(|child| matches!(child.kind(), "anchor_name" | "alias_name"));
        let text = named.map_or_else(|| self.slice(node), |name| self.slice(name));
        text.trim_start_matches(['&', '*']).to_string()
    }

    fn content(&mut self, at: TsNode, indent: i64, depth: usize) -> Node {
        match at.kind() {
            "block_mapping" => {
                let mut entries = Vec::new();
                for pair in syntax::children(at) {
                    if pair.kind() == "block_mapping_pair" {
                        entries.push(self.pair(pair, depth));
                    }
                }
                self.collection(at, Value::Map(entries))
            }
            "block_sequence" => {
                let mut items = Vec::new();
                for item in syntax::children(at) {
                    if item.kind() != "block_sequence_item" {
                        continue;
                    }
                    let column = i64::from(item.start_position().column as u32);
                    items.push(match syntax::children(item).into_iter().next() {
                        Some(value) => self.node(value, column, depth + 1),
                        None => Node::null(syntax::span(item)),
                    });
                }
                self.collection(at, Value::Seq(items))
            }
            "flow_mapping" => {
                let mut entries = Vec::new();
                for child in syntax::children(at) {
                    match child.kind() {
                        "flow_pair" => entries.push(self.pair(child, depth)),
                        "flow_node" => {
                            let key = self.key(child, -1, depth);
                            let value = Node::null(key.span.clone());
                            entries.push(Entry { key, value });
                        }
                        _ => {}
                    }
                }
                self.collection(at, Value::Map(entries))
            }
            "flow_sequence" => {
                let mut items = Vec::new();
                for child in syntax::children(at) {
                    match child.kind() {
                        "flow_pair" => {
                            let entry = self.pair(child, depth);
                            items.push(Node {
                                span: syntax::span(child),
                                value: Value::Map(vec![entry]),
                                templated: false,
                            });
                        }
                        "flow_node" => items.push(self.node(child, -1, depth + 1)),
                        _ => {}
                    }
                }
                self.collection(at, Value::Seq(items))
            }
            "plain_scalar" => self.plain(at),
            "double_quote_scalar" => {
                let folded = fold_flow(self.inner(at), true);
                self.scalar(at, unescape(&folded), ScalarKind::Str, true)
            }
            "single_quote_scalar" => {
                let folded = fold_flow(self.inner(at), false);
                self.scalar(at, folded.replace("''", "'"), ScalarKind::Str, true)
            }
            "block_scalar" => {
                let text = block_scalar(self.slice(at), indent);
                self.scalar(at, text, ScalarKind::Str, false)
            }
            "alias" => self.alias(at),
            _ => Node::null(syntax::span(at)),
        }
    }

    fn collection(&self, at: TsNode, value: Value) -> Node {
        Node {
            span: syntax::span(at),
            value,
            templated: false,
        }
    }

    /// A quoted scalar without its quotes.
    fn inner(&self, at: TsNode) -> &str {
        let raw = self.slice(at);
        raw.get(1..raw.len().saturating_sub(1)).unwrap_or(raw)
    }

    fn plain(&self, at: TsNode) -> Node {
        let text = fold_flow(self.slice(at), false);
        let kind = at.named_child(0).map(|child| child.kind());
        match kind {
            Some("null_scalar") => Node::null(syntax::span(at)),
            Some("boolean_scalar") => self.scalar(at, text, ScalarKind::Bool, false),
            Some("integer_scalar") => self.scalar(at, text, ScalarKind::Int, false),
            Some("float_scalar") => self.scalar(at, text, ScalarKind::Float, false),
            _ => self.scalar(at, text, ScalarKind::Str, false),
        }
    }

    fn alias(&mut self, at: TsNode) -> Node {
        let name = self.anchor_name(at);
        let Some(found) = self.anchors.get(&name) else {
            let line = syntax::lines_of(at).0;
            self.guard.flaw(Flaw::UnresolvedAlias { name, line });
            return Node::null(syntax::span(at));
        };
        let size = found.size();
        if !self.guard.take(size) {
            return Node::null(syntax::span(at));
        }
        found.clone()
    }

    /// A `block_mapping_pair` or `flow_pair`.
    fn pair(&mut self, pair: TsNode, depth: usize) -> Entry {
        let column = i64::from(pair.start_position().column as u32);
        let key = match pair.child_by_field_name("key") {
            Some(key) => self.key(key, column, depth),
            None => {
                let (start, line) = (pair.start_byte() as u32, syntax::lines_of(pair).0);
                Key {
                    text: String::new(),
                    span: Span::new(start..start, (line, line)),
                    templated: false,
                }
            }
        };
        let value = match pair.child_by_field_name("value") {
            Some(value) => self.node(value, column, depth + 1),
            None => Node::null(key.span.clone()),
        };
        Entry { key, value }
    }

    /// A scalar key by its text, any other key (`? [a, b]`) as written.
    fn key(&mut self, at: TsNode, indent: i64, depth: usize) -> Key {
        let node = self.node(at, indent, depth + 1);
        let text = match &node.value {
            Value::Scalar(scalar) => scalar.text.clone(),
            _ => self.slice(at).trim().to_string(),
        };
        Key {
            text,
            span: node.span,
            templated: node.templated,
        }
    }
}

/// A flow scalar's lines folded: a line break is a space, `n` blank lines are
/// `n` line breaks; in a double-quoted scalar a `\` at the end of a line joins
/// it to the next without a space.
fn fold_flow(raw: &str, escaped: bool) -> String {
    if !raw.contains('\n') {
        return raw.to_string();
    }
    let lines: Vec<&str> = raw
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    let last = lines.len() - 1;
    let mut out = String::new();
    let mut blanks = 0;
    let mut glued = false;
    for (index, &line) in lines.iter().enumerate() {
        let mut text = if index == 0 { line } else { line.trim_start() };
        let mut continues = false;
        if index < last {
            if escaped && ends_in_escape(text) {
                text = &text[..text.len() - 1];
                continues = true;
            } else {
                text = text.trim_end();
            }
        }
        if index > 0 {
            if text.is_empty() && index < last {
                blanks += 1;
                continue;
            }
            if glued {
                // joined without a separator
            } else if blanks == 0 {
                out.push(' ');
            } else {
                out.push_str(&"\n".repeat(blanks));
            }
            blanks = 0;
        }
        out.push_str(text);
        glued = continues;
    }
    out
}

/// Whether the text ends in a backslash that is not itself escaped.
fn ends_in_escape(text: &str) -> bool {
    text.bytes().rev().take_while(|byte| *byte == b'\\').count() % 2 == 1
}

/// The escape sequences of a double-quoted scalar. An unknown one is kept as
/// written.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('0') => out.push('\0'),
            Some('a') => out.push('\u{7}'),
            Some('b') => out.push('\u{8}'),
            Some('t' | '\t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('v') => out.push('\u{b}'),
            Some('f') => out.push('\u{c}'),
            Some('r') => out.push('\r'),
            Some('e') => out.push('\u{1b}'),
            Some(' ') => out.push(' '),
            Some('"') => out.push('"'),
            Some('/') => out.push('/'),
            Some('\\') => out.push('\\'),
            Some('N') => out.push('\u{85}'),
            Some('_') => out.push('\u{a0}'),
            Some('L') => out.push('\u{2028}'),
            Some('P') => out.push('\u{2029}'),
            Some('x') => push_code(&mut chars, 2, 'x', &mut out),
            Some('u') => push_code(&mut chars, 4, 'u', &mut out),
            Some('U') => push_code(&mut chars, 8, 'U', &mut out),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// `\xXX`, `\uXXXX`, `\UXXXXXXXX`: the character, or the sequence as written
/// when it names none.
fn push_code(chars: &mut std::str::Chars<'_>, digits: usize, letter: char, out: &mut String) {
    let ahead: String = chars.clone().take(digits).collect();
    let character = match u32::from_str_radix(&ahead, 16) {
        Ok(code) if ahead.chars().count() == digits => char::from_u32(code),
        _ => None,
    };
    if let Some(character) = character {
        out.push(character);
        for _ in 0..digits {
            chars.next();
        }
    } else {
        out.push('\\');
        out.push(letter);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Chomp {
    Strip,
    Clip,
    Keep,
}

/// A block scalar (`|` literal, `>` folded) from its header to its last line.
/// `indent` is the column of the entry or item it is the value of: an explicit
/// indentation indicator counts from it, and without one the first non-empty
/// line gives the content's indentation. The text ends at the end of the last
/// line, as the clip and keep indicators read it.
fn block_scalar(raw: &str, indent: i64) -> String {
    let mut lines = raw.split('\n');
    let header = lines.next().unwrap_or(raw);
    let (folded, headed) = (header.starts_with('>'), header.starts_with(['|', '>']));
    let mut chomp = Chomp::Clip;
    let mut explicit = None;
    if headed {
        for indicator in header.chars().skip(1) {
            match indicator {
                '-' => chomp = Chomp::Strip,
                '+' => chomp = Chomp::Keep,
                '1'..='9' => explicit = indicator.to_digit(10),
                _ => break,
            }
        }
    }
    let mut body: Vec<&str> = if headed {
        lines.collect()
    } else {
        raw.split('\n').collect()
    };
    body = body
        .into_iter()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    if body.last() == Some(&"") {
        body.pop();
    }
    let width = match explicit {
        Some(digits) => (indent + i64::from(digits)).max(0) as usize,
        None => body
            .iter()
            .find(|line| !line.trim().is_empty())
            .map_or(0, |line| line.len() - line.trim_start_matches(' ').len()),
    };
    let mut rows: Vec<String> = body
        .iter()
        .map(|line| {
            let lead = line.len() - line.trim_start_matches(' ').len();
            if line.trim().is_empty() && lead <= width {
                String::new()
            } else if lead >= width {
                line[width..].to_string()
            } else {
                line.trim_start().to_string()
            }
        })
        .collect();
    let mut trailing = 0;
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
        trailing += 1;
    }
    let mut out = if folded {
        fold_block(&rows)
    } else {
        rows.join("\n")
    };
    if rows.is_empty() {
        if chomp == Chomp::Keep {
            out = "\n".repeat(trailing);
        }
    } else {
        match chomp {
            Chomp::Strip => {}
            Chomp::Clip => out.push('\n'),
            Chomp::Keep => {
                out.push('\n');
                out.push_str(&"\n".repeat(trailing));
            }
        }
    }
    out
}

/// A folded scalar's lines: two lines of text join with a space, a blank line
/// between them is a line break, and a line that starts with a space keeps the
/// breaks around it.
fn fold_block(rows: &[String]) -> String {
    let spaced = |line: &str| line.starts_with([' ', '\t']);
    let mut out = String::new();
    let mut previous: Option<&str> = None;
    let mut blanks = 0;
    for row in rows {
        if row.is_empty() {
            blanks += 1;
            continue;
        }
        match previous {
            None => out.push_str(&"\n".repeat(blanks)),
            Some(before) if spaced(before) || spaced(row) => {
                out.push('\n');
                out.push_str(&"\n".repeat(blanks));
            }
            Some(_) if blanks == 0 => out.push(' '),
            Some(_) => out.push_str(&"\n".repeat(blanks)),
        }
        out.push_str(row);
        previous = Some(row);
        blanks = 0;
    }
    out
}
