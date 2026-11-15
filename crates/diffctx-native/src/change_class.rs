//! What kind of change a file carries, stated explicitly so the selection
//! policy can rank evidence by it and the artifact can say what it decided.
//!
//! A range that mixes an image updater's one-line `tag:` bumps with a
//! hand-written manifest rewrite used to lose the manifests: every core
//! competed on token cost and the cheap bumps won (#263). The class is a
//! priority, never a filter — a file of any class stays in the inventory —
//! and `Unknown` is treated like `Content`, because a wrong "mechanical"
//! verdict on a real one-line fix would cost the reader the fix.

use once_cell::sync::Lazy;
use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::changed_lines::{HunkText, reverted};
use crate::types::DiffHunk;

#[derive(
    Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum ChangeClass {
    /// Hand-written, multi-line or otherwise substantive.
    Content,
    /// Not classified with confidence; ranked with `Content`.
    Unknown,
    /// One-line pin/tag/version/digest edits of the kind an updater writes.
    Mechanical,
    /// A file whose header says it is generated.
    Generated,
    /// Only whitespace, comments, quote style, trailing commas, semicolons
    /// or import order changed: a formatter run, not a change a caller sees.
    Layout,
}

/// A single-line change that names a version, a tag, a digest or a hash.
static MECHANICAL_LINE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?ix)
        (^\s*(tag|version|rev|ref|sha|digest|image|newTag|newName|commit|checksum|integrity)\s*[:=])
        | (sha256:[0-9a-f]{12,})
        | (\bv?\d+\.\d+\.\d+(?:[-+.][0-9A-Za-z.]+)?\b)
        | (\b[0-9a-f]{7,64}\b)
        "#,
    )
    .expect("mechanical line regex")
});

pub fn classify(
    hunks: &[&DiffHunk],
    changed_lines: &[String],
    generated: bool,
    layout: bool,
) -> (ChangeClass, &'static str) {
    if generated {
        return (ChangeClass::Generated, "generated-file header");
    }
    if hunks.is_empty() {
        return (ChangeClass::Unknown, "no hunks");
    }
    if layout {
        return (
            ChangeClass::Layout,
            "whitespace, comments, quotes or import order only",
        );
    }
    let single_line = hunks.iter().all(|h| h.new_len <= 1 && h.old_len <= 1);
    if !single_line {
        return (ChangeClass::Content, "multi-line hunks");
    }
    if changed_lines.is_empty() {
        return (ChangeClass::Unknown, "single-line hunks, no diff text");
    }
    if changed_lines.iter().all(|l| MECHANICAL_LINE_RE.is_match(l)) {
        (
            ChangeClass::Mechanical,
            "single-line version/tag/digest edits",
        )
    } else {
        (ChangeClass::Unknown, "single-line hunks")
    }
}

/// Settles `layout` on the runs of one file from its text after the change.
/// The text before is that text with the runs reverted, so both versions
/// parse whole and a run is read in its context: the block a line sits in,
/// the string a line continues, the statement a line ends. A run is layout
/// when its syntax tree is unchanged except for what a formatter rewrites —
/// comments and docstrings, quote style, `;` and trailing commas, the order
/// of imports and of the names one imports. Compared line by line, a dedent
/// out of a loop, a tuple's comma, a rune turned string or the inside of a
/// multi-line string all read as layout (#442). Without a grammar, a clean
/// parse or the text, only trailing whitespace and blank lines are.
pub fn settle_layout(runs: &mut [HunkText], new: Option<&str>) {
    let Some(path) = runs.first().map(|r| r.path.clone()) else {
        return;
    };
    let parsed = new.and_then(|new| {
        let old = reverted(new, runs)?;
        let (old_tree, grammar) = crate::parsers::parse_file(&path, &old)?;
        let (new_tree, _) = crate::parsers::parse_file(&path, new)?;
        (!old_tree.root_node().has_error() && !new_tree.root_node().has_error()).then_some((
            old,
            old_tree,
            new_tree,
            grammar == "python",
        ))
    });
    for run in runs.iter_mut() {
        run.layout = match (&parsed, new) {
            (Some((old, old_tree, new_tree, python)), Some(new)) => {
                let rows = |start: u32, len: u32| (start.max(1) - 1, len);
                Shape::of(old_tree, old, rows(run.old_start, run.old_len), *python)
                    == Shape::of(new_tree, new, rows(run.new_start, run.new_len), *python)
            }
            _ => {
                let text = |lines: &[String]| -> Vec<String> {
                    lines
                        .iter()
                        .map(|l| l.trim_end().to_string())
                        .filter(|l| !l.is_empty())
                        .collect()
                };
                text(&run.removed) == text(&run.added)
            }
        };
    }
}

/// A token of a run's shape: a syntax node opening or closing, or text — a
/// leaf, or what a node holds between its children.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Token {
    Open(u16),
    Close,
    Text(u16, String),
}

/// Every quote mark reads as one: `'a'` and `"a"` are one string wherever
/// the grammar gives both the same node, and a grammar that tells them
/// apart (a Go rune, a shell raw string, a PHP interpolated string) keeps
/// the difference in its node kinds.
const QUOTE: u16 = u16::MAX;
const OWN_TEXT: u16 = u16::MAX - 1;

/// The syntax over some rows of a file: the import statements as a set,
/// and the tokens of everything else in order.
#[derive(PartialEq, Eq)]
struct Shape {
    imports: Vec<Vec<Token>>,
    tokens: Vec<Token>,
}

struct Rows<'a> {
    source: &'a str,
    first: usize,
    last: usize,
    python: bool,
}

impl Shape {
    /// `rows` is the first row (0-based) and the number of rows.
    fn of(tree: &tree_sitter::Tree, source: &str, rows: (u32, u32), python: bool) -> Self {
        let mut shape = Self {
            imports: Vec::new(),
            tokens: Vec::new(),
        };
        if rows.1 > 0 {
            let within = Rows {
                source,
                first: rows.0 as usize,
                last: (rows.0 + rows.1 - 1) as usize,
                python,
            };
            within.node(
                tree.root_node(),
                false,
                &mut shape.tokens,
                &mut shape.imports,
            );
        }
        shape.imports.sort();
        shape
    }
}

/// Statements that only bring names into scope, which organise-imports
/// reorders.
fn is_import(kind: &str) -> bool {
    matches!(
        kind,
        "import_statement"
            | "import_from_statement"
            | "future_import_statement"
            | "import_declaration"
            | "import_header"
            | "use_declaration"
            | "using_directive"
            | "preproc_include"
            | "namespace_use_declaration"
    )
}

impl Rows<'_> {
    fn covers(&self, start: tree_sitter::Point, end: tree_sitter::Point) -> bool {
        // A node ending at the start of a row ends on the row above it.
        let last = if end.column == 0 && end.row > start.row {
            end.row - 1
        } else {
            end.row
        };
        start.row <= self.last && last >= self.first
    }

    fn node(
        &self,
        node: tree_sitter::Node,
        in_import: bool,
        out: &mut Vec<Token>,
        imports: &mut Vec<Vec<Token>>,
    ) {
        if node.child_count() == 0 {
            let text = &self.source[node.byte_range()];
            out.push(match (node.is_named(), node.kind()) {
                (false, _) if matches!(text, "'" | "\"") => Token::Text(QUOTE, String::new()),
                (true, "string_start" | "string_end") if self.python => {
                    Token::Text(node.kind_id(), text.replace('\'', "\""))
                }
                _ => Token::Text(node.kind_id(), text.to_string()),
            });
            return;
        }
        if node.is_named() && !in_import && is_import(node.kind()) {
            let mut unit = Vec::new();
            self.node(node, true, &mut unit, imports);
            if !unit.is_empty() {
                imports.push(unit);
            }
            return;
        }
        let mark = out.len();
        if node.is_named() {
            out.push(Token::Open(node.kind_id()));
        }
        self.children(node, in_import, out, imports);
        if node.is_named() {
            if out.len() == mark + 1 {
                out.pop();
            } else {
                out.push(Token::Close);
            }
        }
    }

    /// The children within the rows and the node's own text between them.
    /// Inside an import, the names a list holds compare as a set.
    fn children(
        &self,
        node: tree_sitter::Node,
        in_import: bool,
        out: &mut Vec<Token>,
        imports: &mut Vec<Vec<Token>>,
    ) {
        let mut cursor = node.walk();
        let kids: Vec<tree_sitter::Node> = node.children(&mut cursor).collect();
        let mut names: Vec<Vec<Token>> = Vec::new();
        let flush = |names: &mut Vec<Vec<Token>>, out: &mut Vec<Token>| {
            names.sort();
            out.extend(names.drain(..).flatten());
        };
        let (mut gap, mut gap_at) = (node.start_byte(), node.start_position());
        for i in 0..=kids.len() {
            let (end, end_at) = kids.get(i).map_or_else(
                || (node.end_byte(), node.end_position()),
                |k| (k.start_byte(), k.start_position()),
            );
            let own = &self.source[gap..end.max(gap)];
            if !own.trim().is_empty() && self.covers(gap_at, end_at) {
                flush(&mut names, out);
                out.push(Token::Text(OWN_TEXT, own.to_string()));
            }
            let Some(&kid) = kids.get(i) else { break };
            (gap, gap_at) = (kid.end_byte(), kid.end_position());
            if !self.covers(kid.start_position(), kid.end_position())
                || kid.kind().contains("comment")
                || self.is_docstring(kid)
            {
                continue;
            }
            if !kid.is_named() {
                let closes = || {
                    kids[i + 1..]
                        .iter()
                        .find(|k| !k.kind().contains("comment"))
                        .is_some_and(|k| !k.is_named() && matches!(k.kind(), ")" | "]" | "}"))
                };
                match kid.kind() {
                    ";" => continue,
                    "," if in_import || (closes() && !self.makes_a_tuple(node)) => continue,
                    _ => {}
                }
            }
            if in_import && kid.is_named() {
                let mut name = Vec::new();
                self.node(kid, true, &mut name, imports);
                names.push(name);
                continue;
            }
            flush(&mut names, out);
            self.node(kid, in_import, out, imports);
        }
        flush(&mut names, out);
    }

    /// A docstring documents, like a comment: the string that opens a
    /// module, class or function body.
    fn is_docstring(&self, node: tree_sitter::Node) -> bool {
        if !self.python || node.kind() != "expression_statement" {
            return false;
        }
        let mut cursor = node.walk();
        let mut inner = node.named_children(&mut cursor);
        let (Some(only), None) = (inner.next(), inner.next()) else {
            return false;
        };
        let Some(body) = node.parent() else {
            return false;
        };
        let opens_body = body.kind() == "module"
            || (body.kind() == "block"
                && body.parent().is_some_and(|d| {
                    matches!(d.kind(), "function_definition" | "class_definition")
                }));
        let mut cursor = body.walk();
        let first = body
            .named_children(&mut cursor)
            .find(|s| !s.kind().contains("comment"));
        matches!(only.kind(), "string" | "concatenated_string") && opens_body && first == Some(node)
    }

    /// `d[1,]` indexes with the tuple `(1,)`: in a Python subscript with one
    /// index the trailing comma is the tuple. Elsewhere a tuple is its own
    /// node and its comma changes the node kind.
    fn makes_a_tuple(&self, node: tree_sitter::Node) -> bool {
        let mut cursor = node.walk();
        self.python
            && node.kind() == "subscript"
            && node
                .children_by_field_name("subscript", &mut cursor)
                .count()
                == 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn hunk(new_len: u32, old_len: u32) -> DiffHunk {
        DiffHunk {
            path: Arc::from("x"),
            new_start: 1,
            new_len,
            old_start: 1,
            old_len,
        }
    }

    #[test]
    fn a_tag_bump_is_mechanical_and_a_one_line_fix_is_unknown() {
        let h = hunk(1, 1);
        let bump = vec![
            "    tag: main-abc1234".into(),
            "    tag: main-def5678".into(),
        ];
        assert_eq!(
            classify(&[&h], &bump, false, false).0,
            ChangeClass::Mechanical
        );
        let fix = vec!["    return x + 1".into(), "    return x + 2".into()];
        assert_eq!(classify(&[&h], &fix, false, false).0, ChangeClass::Unknown);
        let digest = vec!["image: r/app@sha256:0123456789abcdef".into()];
        assert_eq!(
            classify(&[&h], &digest, false, false).0,
            ChangeClass::Mechanical
        );
    }

    #[test]
    fn multi_line_hunks_are_content_and_generated_wins() {
        let h = hunk(5, 2);
        assert_eq!(classify(&[&h], &[], false, false).0, ChangeClass::Content);
        assert_eq!(classify(&[&h], &[], true, false).0, ChangeClass::Generated);
    }
}
