//! What the readers share: a tree-sitter parse that stops with the run, the
//! spans of its nodes, the line ranges it could not read, a line index for the
//! readers that do not parse with a grammar, and the guard that bounds every
//! walk of an input.

use std::cell::RefCell;

use tree_sitter::{Language as TsLanguage, Node as TsNode, ParseOptions, ParseState, Parser, Tree};

use crate::facts::{Span, expired};
use crate::snapshot::RunBudget;

use super::Flaw;

/// Nesting deeper than this is not read: the walks recurse.
pub(super) const MAX_DEPTH: usize = 100;
/// Nodes one file may make, aliases included, before the rest is left unread.
pub(super) const MAX_NODES: usize = 400_000;
/// Nodes between two looks at the run's deadline.
const POLL_EVERY: usize = 256;
/// Error ranges kept for a file.
const ERROR_RANGES_KEPT: usize = 16;

thread_local! {
    static PARSER: RefCell<Option<Parser>> = const { RefCell::new(None) };
}

/// A parse that ends when the run's budget does: `None` for a grammar that does
/// not load or a parse the budget stopped.
#[allow(
    clippy::disallowed_methods,
    reason = "a grammar that does not load or a parse that ends is `None`, which the reader turns into a named flaw; past the end the read callback gives no bytes"
)]
pub(crate) fn parse(grammar: &TsLanguage, text: &[u8], budget: &RunBudget) -> Option<Tree> {
    PARSER.with(|slot| {
        let mut slot = slot.borrow_mut();
        let parser = slot.get_or_insert_with(Parser::new);
        parser.set_language(grammar).ok()?;
        let mut progress = |_: &ParseState| expired(budget);
        let len = text.len();
        let tree = parser.parse_with_options(
            &mut |i, _| (i < len).then(|| &text[i..]).unwrap_or_default(),
            None,
            Some(ParseOptions::new().progress_callback(&mut progress)),
        );
        if tree.is_none() {
            parser.reset();
        }
        tree
    })
}

pub(crate) fn lines_of(node: TsNode) -> (u32, u32) {
    let start = node.start_position();
    let end = node.end_position();
    let first = start.row as u32 + 1;
    let mut last = end.row as u32 + 1;
    if end.column == 0 && end.row > start.row {
        last -= 1;
    }
    (first, last.max(first))
}

pub(crate) fn span(node: TsNode) -> Span {
    Span {
        bytes: node.start_byte() as u32..node.end_byte() as u32,
        lines: lines_of(node),
    }
}

/// The line ranges the grammar could not read, merged.
pub(crate) fn error_lines(root: TsNode) -> Vec<(u32, u32)> {
    let mut ranges = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_error() || node.is_missing() {
            ranges.push(lines_of(node));
        } else if node.has_error() {
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
    }
    merge_ranges(ranges)
}

pub(super) fn merge_ranges(ranges: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    crate::facts::lang::merge_ranges(ranges, ERROR_RANGES_KEPT)
}

/// The named children of a node that are not comments, in order.
pub(crate) fn children(node: TsNode<'_>) -> Vec<TsNode<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| child.kind() != "comment")
        .collect()
}

/// Byte offsets to lines, for the readers whose library reports bytes.
pub(super) struct LineIndex {
    starts: Vec<u32>,
}

impl LineIndex {
    pub(super) fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            text.bytes()
                .enumerate()
                .filter(|(_, byte)| *byte == b'\n')
                .map(|(at, _)| at as u32 + 1),
        );
        Self { starts }
    }

    pub(super) fn line_of(&self, byte: u32) -> u32 {
        self.starts.partition_point(|start| *start <= byte) as u32
    }

    pub(super) fn count(&self) -> u32 {
        self.starts.len() as u32
    }

    /// The span of `start..end`; its last line is the one of its last byte.
    pub(super) fn span(&self, start: usize, end: usize) -> Span {
        let (start, end) = (start as u32, end as u32);
        let last = if end > start { end - 1 } else { start };
        Span {
            bytes: start..end,
            lines: (self.line_of(start), self.line_of(last)),
        }
    }
}

/// Bounds a walk: depth, the nodes made, the run's deadline.
pub(super) struct Guard<'a> {
    budget: &'a RunBudget,
    nodes: usize,
    pub(super) halted: bool,
    pub(super) flaws: Vec<Flaw>,
}

impl<'a> Guard<'a> {
    pub(super) fn new(budget: &'a RunBudget) -> Self {
        Self {
            budget,
            nodes: 0,
            halted: false,
            flaws: Vec::new(),
        }
    }

    pub(super) fn flaw(&mut self, flaw: Flaw) {
        if !self.flaws.contains(&flaw) {
            self.flaws.push(flaw);
        }
    }

    /// Whether a node at `depth` may be read; a refusal is recorded as a flaw.
    pub(super) fn enter(&mut self, depth: usize) -> bool {
        self.nodes += 1;
        if depth > MAX_DEPTH {
            self.flaw(Flaw::TooDeep);
            return false;
        }
        if self.nodes > MAX_NODES {
            self.flaw(Flaw::TooLarge);
            return false;
        }
        if self.nodes % POLL_EVERY == 0 && expired(self.budget) {
            self.halted = true;
            self.flaw(Flaw::Deadline);
        }
        !self.halted
    }

    /// Takes `count` nodes for a copy (an alias); `false` when they do not fit.
    pub(super) fn take(&mut self, count: usize) -> bool {
        if self.nodes + count > MAX_NODES {
            self.flaw(Flaw::TooLarge);
            return false;
        }
        self.nodes += count;
        true
    }

    /// The deadline, looked at between lines of a reader that makes no nodes.
    pub(super) fn poll(&mut self, line: usize) -> bool {
        if line % POLL_EVERY == 0 && expired(self.budget) {
            self.halted = true;
            self.flaw(Flaw::Deadline);
        }
        !self.halted
    }
}
